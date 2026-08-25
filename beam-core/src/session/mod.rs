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

use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};

pub use self::connector::ConnectError;
pub use self::framebuffer::Framebuffer;
pub use self::input::{InputEvent, PointerButton};

use self::clipboard::ClipboardBridge;
use crate::events::{CredentialsPromptRequest, DisconnectReason, SessionEvent};
use crate::profile::ConnectionProfile;
use crate::secrets::{self, SecretKey};

/// Commands a frontend can send into a running session. Internal plumbing only — a frontend
/// never constructs these directly, it goes through [`SessionHandle`]'s methods.
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
}

impl SessionEvents {
    /// Await the next session event. Returns `None` once the session task has fully exited
    /// (always preceded by a [`SessionEvent::Disconnected`]).
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        self.events.recv().await
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
    cancelled: watch::Sender<bool>,
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
        // Discrete transitions must remain ordered and must not be discarded. This bounded queue
        // applies backpressure only when the network has fallen more than 512 transitions behind.
        if self
            .essential_commands
            .blocking_send(SessionCommand::Input(event))
            .is_err()
        {
            debug!("sessão encerrada antes do envio de entrada discreta");
        }
    }

    pub fn send_ctrl_alt_del(&self) {
        if self
            .essential_commands
            .blocking_send(SessionCommand::CtrlAltDel)
            .is_err()
        {
            warn!("sessão encerrada antes do envio de Ctrl+Alt+Del");
        }
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
        let _ = self.cancelled.send(true);
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
    let (essential_tx, essential_rx) = mpsc::channel(512);
    let (pointer_tx, pointer_rx) = watch::channel(None);
    let (clipboard_tx, clipboard_rx) = watch::channel(0);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let framebuffer = Arc::new(Framebuffer::new(
        profile.resolution.width,
        profile.resolution.height,
    ));
    let clipboard_bridge = Arc::new(ClipboardBridge::default());

    let task_framebuffer = framebuffer.clone();
    let task_clipboard_bridge = clipboard_bridge.clone();
    runtime.spawn(async move {
        run_session(
            profile,
            events_tx,
            commands_rx,
            essential_rx,
            pointer_rx,
            clipboard_rx,
            task_framebuffer,
            task_clipboard_bridge,
            cancel_rx,
        )
        .await;
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
        SessionEvents { events: events_rx },
    )
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
    mut cancelled: watch::Receiver<bool>,
) {
    let username = profile.username.clone();
    let key = SecretKey {
        host: profile.normalized_host(),
        port: profile.port,
        user: &username,
    };

    let lookup = tokio::select! {
        result = secrets::lookup_password(&key) => result,
        _ = cancelled.changed() => {
            disconnect_cancelled(&events).await;
            return;
        }
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
                _ = cancelled.changed() => {
                    disconnect_cancelled(&events).await;
                    return;
                }
            };
            match answer {
                Some((password, save)) => {
                    save_after_auth = save;
                    password
                }
                _ => {
                    let _ = events
                        .send(SessionEvent::Disconnected(
                            DisconnectReason::ConnectionFailed("senha não fornecida".to_owned()),
                        ))
                        .await;
                    return;
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
            .await;
        }
        Err(e) => {
            if credential_from_store && e.is_authentication_rejected() {
                if let Err(error) = secrets::delete_password(&key).await {
                    warn!(%error, "falha ao invalidar credencial rejeitada");
                }
            }
            let _ = events
                .send(SessionEvent::Disconnected(
                    DisconnectReason::ConnectionFailed(e.to_string()),
                ))
                .await;
        }
    }
}

async fn disconnect_cancelled(events: &mpsc::Sender<SessionEvent>) {
    let _ = events
        .send(SessionEvent::Disconnected(DisconnectReason::UserInitiated))
        .await;
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
        let (essential, essential_rx) = mpsc::channel(2);
        let (pointer, pointer_rx) = watch::channel(None);
        let (clipboard_generation, _) = watch::channel(0);
        let (cancelled, _) = watch::channel(false);
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
}
