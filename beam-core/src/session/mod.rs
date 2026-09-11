//! Public API surface of the session engine: [`connect`] starts a session and hands back a
//! [`SessionController`] (commands + framebuffer) and [`SessionEvents`] (the event stream) — the
//! only things a frontend needs to drive an RDP session. No IronRDP type ever crosses this
//! boundary.

mod active;
mod clipboard;
mod connector;
pub mod framebuffer;
mod input;

use std::sync::Arc;

use std::future::Future;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};

pub use self::connector::ConnectError;
pub use self::framebuffer::Framebuffer;
pub use self::input::{InputEvent, PointerButton};

use self::clipboard::ClipboardBridge;
use crate::events::{CredentialsPromptRequest, DisconnectReason, SessionEvent};
use crate::profile::ConnectionProfile;
use crate::secrets::{self, SecretKey};

const ESSENTIAL_QUEUE_CAPACITY: usize = 4096;

/// Commands a frontend can send into a running session. Internal plumbing only — a frontend
/// never constructs these directly, it goes through [`SessionController`]'s methods.
pub(crate) enum SessionCommand {
    Input(InputEvent),
    CtrlAltDel,
    Disconnect,
}

/// The receiving half of a session: a move-only stream of [`SessionEvent`]s.
///
/// Kept separate from [`SessionController`] specifically so a frontend never needs to share a
/// single `SessionHandle` behind a `RefCell` to poll events from one task while sending commands
/// from UI callbacks on the same thread — doing so risks a `RefCell` borrow panic the moment a
/// callback fires while the event-pump task is suspended mid-`.await` holding a borrow. With the
/// split, the event pump owns its receiver outright and every other closure just clones the
/// cheap, `Send`-free [`SessionController`].
pub struct SessionEvents {
    events: mpsc::Receiver<SessionEvent>,
    finished: Option<oneshot::Receiver<DisconnectReason>>,
}

impl SessionEvents {
    /// Await the next session event. Returns `None` once the session task has fully exited
    /// (always preceded by a [`SessionEvent::Disconnected`]).
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        let finished = self.finished.as_mut()?;
        let reason = tokio::select! {
            biased;
            reason = &mut *finished => reason,
            event = self.events.recv() => {
                if event.is_some() { return event; }
                finished.await
            }
        };
        self.finished = None;
        self.events.close();
        Some(SessionEvent::Disconnected(reason.unwrap_or_else(|_| {
            DisconnectReason::ConnectionLost("Session ended unexpectedly".into())
        })))
    }
}

/// A cheaply-`Clone`-able handle for sending commands into a running session and reading its
/// framebuffer. See [`SessionEvents`] for why this is a separate type from the event stream.
#[derive(Clone)]
pub struct SessionController {
    commands: mpsc::Sender<SessionCommand>,
    essential_commands: mpsc::Sender<SessionCommand>,
    pointer_position: watch::Sender<Option<InputEvent>>,
    clipboard_generation: watch::Sender<u64>,
    cancelled: watch::Sender<Option<DisconnectReason>>,
    framebuffer: Arc<Framebuffer>,
    clipboard_bridge: Arc<ClipboardBridge>,
}

impl SessionController {
    /// The session's shared framebuffer, for the display widget to snapshot when painting.
    pub fn framebuffer(&self) -> &Arc<Framebuffer> {
        &self.framebuffer
    }

    pub fn send_input(&self, event: InputEvent) {
        if matches!(event, InputEvent::MouseMove { .. }) {
            self.pointer_position.send_replace(Some(event));
            return;
        }
        self.send_essential(SessionCommand::Input(event));
    }

    pub fn send_ctrl_alt_del(&self) {
        self.send_essential(SessionCommand::CtrlAltDel);
    }

    fn send_essential(&self, command: SessionCommand) {
        if self.cancelled.borrow().is_some() {
            return;
        }
        match self.essential_commands.try_send(command) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Never continue a session after losing a key/button transition.
                self.request_stop(DisconnectReason::ConnectionLost(
                    "Input queue full; disconnected to avoid losing key releases".into(),
                ));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                debug!("sessão encerrada antes do envio de entrada discreta");
            }
        }
    }

    fn request_stop(&self, reason: DisconnectReason) {
        self.cancelled.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(reason);
                true
            }
        });
    }

    /// Notify the session that the local (GTK) clipboard now holds `text`, offering it to the
    /// remote desktop.
    pub fn set_local_clipboard_text(&self, text: String) {
        // Write the cache before enqueuing the command: the active-session loop only reads it
        // after it dequeues `LocalClipboardChanged`, and by then this write has already
        // happened-before that dequeue (it happened-before the very send below).
        self.clipboard_bridge.set_local_text(text);
        let next = self.clipboard_generation.borrow().wrapping_add(1);
        self.clipboard_generation.send_replace(next);
    }

    pub fn disconnect(&self) {
        self.request_stop(DisconnectReason::UserInitiated);
        let _ = self.commands.try_send(SessionCommand::Disconnect);
    }
}

/// Start connecting to `profile` in the background, on `runtime`. Returns immediately; watch
/// [`SessionEvents::next_event`] for [`SessionEvent::Connected`], [`SessionEvent::CertPrompt`],
/// [`SessionEvent::CredsNeeded`], and eventually [`SessionEvent::Disconnected`] if the attempt
/// fails.
pub fn connect(
    profile: ConnectionProfile,
    runtime: &tokio::runtime::Handle,
) -> (SessionController, SessionEvents) {
    let (events_tx, events_rx) = mpsc::channel(64);
    let (commands_tx, commands_rx) = mpsc::channel(256);
    let (essential_tx, essential_rx) = mpsc::channel(ESSENTIAL_QUEUE_CAPACITY);
    let (pointer_tx, pointer_rx) = watch::channel(None);
    let (clipboard_tx, clipboard_rx) = watch::channel(0);
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let (finished_tx, finished_rx) = oneshot::channel();
    let framebuffer = Arc::new(Framebuffer::new(
        profile.resolution.width,
        profile.resolution.height,
    ));
    let clipboard_bridge = Arc::new(ClipboardBridge::default());

    let task_framebuffer = framebuffer.clone();
    let task_clipboard_bridge = clipboard_bridge.clone();
    runtime.spawn(async move {
        let session = run_session(
            profile,
            events_tx.clone(),
            commands_rx,
            essential_rx,
            pointer_rx,
            clipboard_rx,
            task_framebuffer,
            task_clipboard_bridge,
            cancel_rx.clone(),
        );
        let reason = supervise(session, &events_tx, cancel_rx).await;
        // Independent of the bounded UI event queue: teardown never waits for GTK.
        let _ = finished_tx.send(reason);
    });

    (
        SessionController {
            commands: commands_tx,
            essential_commands: essential_tx,
            pointer_position: pointer_tx,
            clipboard_generation: clipboard_tx,
            cancelled: cancel_tx,
            framebuffer,
            clipboard_bridge,
        },
        SessionEvents {
            events: events_rx,
            finished: Some(finished_rx),
        },
    )
}

/// Covers every await in the session, including event delivery, keyring operations,
/// TLS writes and activation. Cancelling drops the entire transport future; a partial
/// frame is never retried on the same connection.
async fn supervise(
    session: impl Future<Output = DisconnectReason>,
    events: &mpsc::Sender<SessionEvent>,
    mut cancelled: watch::Receiver<Option<DisconnectReason>>,
) -> DisconnectReason {
    tokio::select! {
        biased;
        reason = wait_for_cancel(&mut cancelled) => reason,
        _ = events.closed() => DisconnectReason::UserInitiated,
        reason = session => reason,
    }
}

pub(super) async fn wait_for_cancel(
    cancelled: &mut watch::Receiver<Option<DisconnectReason>>,
) -> DisconnectReason {
    loop {
        if let Some(reason) = cancelled.borrow_and_update().clone() {
            return reason;
        }
        if cancelled.changed().await.is_err() {
            return DisconnectReason::UserInitiated;
        }
    }
}

// SessionController owns these channels separately so lossy pointer traffic
// cannot starve essential input, cancellation or clipboard state.
#[allow(clippy::too_many_arguments)]
async fn run_session(
    profile: ConnectionProfile,
    events: mpsc::Sender<SessionEvent>,
    commands: mpsc::Receiver<SessionCommand>,
    essential_commands: mpsc::Receiver<SessionCommand>,
    pointer_position: watch::Receiver<Option<InputEvent>>,
    clipboard_generation: watch::Receiver<u64>,
    framebuffer: Arc<Framebuffer>,
    clipboard_bridge: Arc<ClipboardBridge>,
    mut cancelled: watch::Receiver<Option<DisconnectReason>>,
) -> DisconnectReason {
    let username = profile.username.clone();
    let key = SecretKey {
        host: profile.normalized_host(),
        port: profile.port,
        user: &username,
    };

    let lookup = tokio::select! {
        result = secrets::lookup_password(&key) => result,
        reason = wait_for_cancel(&mut cancelled) => return reason
    };
    let mut save_after_auth = false;
    let mut credential_from_store = false;
    let password = match lookup {
        Ok(Some(password)) => {
            credential_from_store = true;
            password
        }
        other => {
            if let Err(e) = other {
                warn!("falha ao consultar o chaveiro do sistema: {e}");
            }

            let (tx, rx) = oneshot::channel();
            let _ = events
                .send(SessionEvent::CredsNeeded(CredentialsPromptRequest {
                    username: username.clone(),
                    respond: tx,
                }))
                .await;

            let answer = tokio::select! {
                answer = rx => answer.ok().flatten(),
                reason = wait_for_cancel(&mut cancelled) => return reason
            };
            match answer {
                Some((password, save)) => {
                    save_after_auth = save;
                    password
                }
                _ => {
                    return DisconnectReason::ConnectionFailed("senha não fornecida".to_owned());
                }
            }
        }
    };

    let (backend_tx, backend_rx) = mpsc::unbounded_channel();
    let backend = clipboard::build_backend(clipboard_bridge, backend_tx);
    let cliprdr = ironrdp_cliprdr::Cliprdr::<ironrdp_cliprdr::Client>::new(backend);

    match connector::connect(
        &profile,
        &username,
        &password,
        cliprdr,
        &events,
        cancelled.clone(),
    )
    .await
    {
        Ok(connected) => {
            if save_after_auth {
                if let Err(e) = secrets::store_password(&key, &password).await {
                    warn!("falha ao salvar senha no chaveiro do sistema: {e}");
                }
            }
            active::run(
                connected.framed,
                connected.connection_result,
                events,
                commands,
                essential_commands,
                pointer_position,
                clipboard_generation,
                cancelled,
                backend_rx,
                framebuffer,
            )
            .await
        }
        Err(e) => {
            if credential_from_store && e.is_authentication_rejected() {
                if let Err(error) = secrets::delete_password(&key).await {
                    warn!(%error, "falha ao invalidar credencial rejeitada");
                }
            }
            DisconnectReason::ConnectionFailed(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_controller() -> (
        SessionController,
        mpsc::Receiver<SessionCommand>,
        mpsc::Receiver<SessionCommand>,
        watch::Receiver<Option<InputEvent>>,
    ) {
        let (commands, commands_rx) = mpsc::channel(2);
        let (essential, essential_rx) = mpsc::channel(ESSENTIAL_QUEUE_CAPACITY);
        let (pointer, pointer_rx) = watch::channel(None);
        let (clipboard_generation, _) = watch::channel(0);
        let (cancelled, _) = watch::channel(None);
        (
            SessionController {
                commands,
                essential_commands: essential,
                pointer_position: pointer,
                clipboard_generation,
                cancelled,
                framebuffer: Arc::new(Framebuffer::new(1, 1)),
                clipboard_bridge: Arc::new(ClipboardBridge::default()),
            },
            commands_rx,
            essential_rx,
            pointer_rx,
        )
    }

    #[test]
    fn mouse_moves_are_coalesced_to_latest_position() {
        let (controller, _commands, _essential, pointer) = test_controller();
        for x in 0..10_000 {
            controller.send_input(InputEvent::MouseMove { x, y: 42 });
        }
        assert_eq!(
            *pointer.borrow(),
            Some(InputEvent::MouseMove { x: 9_999, y: 42 })
        );
    }

    #[test]
    fn discrete_transitions_use_the_reserved_bounded_queue() {
        let (controller, mut commands, mut essential, _pointer) = test_controller();
        controller.send_input(InputEvent::Key {
            scancode: 30,
            extended: false,
            pressed: true,
        });
        controller.send_input(InputEvent::Key {
            scancode: 30,
            extended: false,
            pressed: false,
        });
        assert!(commands.try_recv().is_err());
        assert!(matches!(
            essential.try_recv(),
            Ok(SessionCommand::Input(InputEvent::Key { pressed: true, .. }))
        ));
        assert!(matches!(
            essential.try_recv(),
            Ok(SessionCommand::Input(InputEvent::Key {
                pressed: false,
                ..
            }))
        ));
    }
    #[tokio::test]
    async fn full_input_queue_cancels_without_blocking_or_silently_losing_a_release() {
        let (controller, mut commands, mut essential, _) = test_controller();
        // Filling the ordinary queue must not prevent essential input or cancellation.
        controller
            .commands
            .try_send(SessionCommand::CtrlAltDel)
            .ok()
            .unwrap();
        controller
            .commands
            .try_send(SessionCommand::CtrlAltDel)
            .ok()
            .unwrap();
        for i in 0..ESSENTIAL_QUEUE_CAPACITY {
            controller.send_input(InputEvent::Key {
                scancode: 30,
                extended: false,
                pressed: i % 2 == 0,
            });
        }
        assert!(controller.cancelled.borrow().is_none());
        controller.send_input(InputEvent::Key {
            scancode: 30,
            extended: false,
            pressed: false,
        });
        assert!(
            matches!(&*controller.cancelled.borrow(), Some(DisconnectReason::ConnectionLost(text))
            if text == "Input queue full; disconnected to avoid losing key releases")
        );
        // Later close requests preserve the useful overload diagnostic.
        controller.disconnect();
        assert!(matches!(
            &*controller.cancelled.borrow(),
            Some(DisconnectReason::ConnectionLost(_))
        ));
        assert_eq!(commands.len(), 2);
        assert!(commands.try_recv().is_ok());
        for i in 0..ESSENTIAL_QUEUE_CAPACITY {
            assert!(
                matches!(essential.try_recv(), Ok(SessionCommand::Input(InputEvent::Key { pressed, .. }))
                if pressed == (i % 2 == 0))
            );
        }
        assert!(essential.try_recv().is_err());
    }

    #[tokio::test]
    async fn terminal_event_bypasses_full_event_queue_and_is_delivered_once() {
        let (events, receiver) = mpsc::channel(1);
        events
            .try_send(SessionEvent::Connected {
                width: 1,
                height: 1,
            })
            .unwrap();
        let (finished, finished_rx) = oneshot::channel();
        let (cancel, cancelled) = watch::channel(None);
        let mut stream = SessionEvents {
            events: receiver,
            finished: Some(finished_rx),
        };
        let task = tokio::spawn(async move {
            let blocked = async {
                events
                    .send(SessionEvent::Connected {
                        width: 2,
                        height: 2,
                    })
                    .await
                    .unwrap();
                panic!("the full queue must not be drained");
            };
            let reason = supervise(blocked, &events, cancelled).await;
            finished.send(reason).unwrap();
        });
        tokio::task::yield_now().await;
        cancel.send_replace(Some(DisconnectReason::UserInitiated));
        task.await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(SessionEvent::Disconnected(DisconnectReason::UserInitiated))
        ));
        assert!(stream.next_event().await.is_none());
    }

    #[tokio::test]
    async fn cancellation_already_requested_never_starts_session_work() {
        let (events, _receiver) = mpsc::channel(1);
        let (_cancel, cancelled) = watch::channel(Some(DisconnectReason::UserInitiated));
        let reason = supervise(
            async { panic!("must not access keyring or network") },
            &events,
            cancelled,
        )
        .await;
        assert!(matches!(reason, DisconnectReason::UserInitiated));
    }

    #[tokio::test]
    async fn dropping_all_controllers_cancels_pending_work() {
        let (events, _receiver) = mpsc::channel(1);
        let (cancel, cancelled) = watch::channel(None);
        drop(cancel);
        let reason = supervise(std::future::pending(), &events, cancelled).await;
        assert!(matches!(reason, DisconnectReason::UserInitiated));
    }
}
