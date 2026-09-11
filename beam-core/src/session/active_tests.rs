//! Exercise the actual active RDP loop with a peer that deliberately stops progressing.
use super::*;
use crate::profile::ConnectionProfile;
use crate::session::{supervise, InputEvent, ESSENTIAL_QUEUE_CAPACITY};
use ironrdp_connector::connection_activation::ConnectionActivationFactory;
use ironrdp_connector::DesktopSize;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

struct Peer {
    socket: DuplexStream,
    input: mpsc::Sender<SessionCommand>,
    cancel: watch::Sender<Option<DisconnectReason>>,
    events: mpsc::Receiver<SessionEvent>,
    task: tokio::task::JoinHandle<DisconnectReason>,
    // Keep optional frontend channels alive, just as SessionController does.
    _commands: mpsc::Sender<SessionCommand>,
    _pointer: watch::Sender<Option<InputEvent>>,
    _clipboard: watch::Sender<u64>,
}

impl Peer {
    async fn start() -> Self {
        let (client, socket) = tokio::io::duplex(1);
        let (events_tx, events) = mpsc::channel(64);
        let (commands_tx, commands) = mpsc::channel(1);
        let (input, essential) = mpsc::channel(ESSENTIAL_QUEUE_CAPACITY);
        let (pointer_tx, pointer) = watch::channel(None);
        let (clipboard_tx, clipboard) = watch::channel(0);
        let (cancel, cancelled) = watch::channel(None);
        // No negotiated clipboard channel. Its closed backend must not spin.
        let (_, clipboard_backend) = mpsc::unbounded_channel();
        let config = crate::session::connector::build_config(
            &ConnectionProfile::new("test", "unused", "user"),
            "user",
            "test",
        );
        let result = ConnectionResult {
            io_channel_id: 1003,
            user_channel_id: 1001,
            message_channel_id: None,
            share_id: 1,
            static_channels: Default::default(),
            desktop_size: DesktopSize {
                width: 2,
                height: 2,
            },
            enable_server_pointer: false,
            pointer_software_rendering: false,
            activation_factory: ConnectionActivationFactory::new(config, 1003, 1001),
            compression_type: None,
        };
        let task = tokio::spawn(async move {
            let session = run(
                TokioFramed::new(client),
                result,
                events_tx.clone(),
                commands,
                essential,
                pointer,
                clipboard,
                cancelled.clone(),
                clipboard_backend,
                Arc::new(Framebuffer::new(2, 2)),
            );
            supervise(session, &events_tx, cancelled).await
        });
        let mut peer = Self {
            socket,
            input,
            cancel,
            events,
            task,
            _commands: commands_tx,
            _pointer: pointer_tx,
            _clipboard: clipboard_tx,
        };
        assert!(matches!(
            peer.events.recv().await,
            Some(SessionEvent::Connected { .. })
        ));
        peer
    }

    async fn block_write(&mut self) {
        self.input
            .try_send(SessionCommand::Input(InputEvent::Key {
                scancode: 30,
                extended: false,
                pressed: true,
            }))
            .ok()
            .unwrap();
        let mut byte = [0];
        // A real encoded fast-path frame has begun, but the one-byte pipe cannot finish it.
        self.socket.read_exact(&mut byte).await.unwrap();
        assert!(!self.task.is_finished());
    }

    async fn deactivate(&mut self) {
        use ironrdp_pdu::rdp::headers::{ServerDeactivateAll, ShareControlHeader, ShareControlPdu};
        let payload = ironrdp_core::encode_vec(&ShareControlHeader {
            share_control_pdu: ShareControlPdu::ServerDeactivateAll(ServerDeactivateAll),
            pdu_source: 1001,
            share_id: 1,
        })
        .unwrap();
        let pdu = ironrdp_core::encode_vec(&ironrdp_pdu::x224::X224(
            ironrdp_pdu::mcs::SendDataIndication {
                initiator_id: 1001,
                channel_id: 1003,
                user_data: payload.into(),
            },
        ))
        .unwrap();
        self.socket.write_all(&pdu).await.unwrap();
        // Start an incomplete TPKT packet; the activation must time out even mid-header.
        self.socket.write_all(&[3]).await.unwrap();
        assert!(!self.task.is_finished());
    }
}

#[tokio::test(start_paused = true)]
async fn nonreading_peer_hits_write_deadline_and_transport_closes() {
    let mut peer = Peer::start().await;
    peer.block_write().await;
    let reason = peer.task.await.unwrap();
    assert!(
        matches!(reason, DisconnectReason::ConnectionLost(ref text) if text == "Timed out sending data")
    );
    let mut remainder = Vec::new();
    peer.socket.read_to_end(&mut remainder).await.unwrap();
    assert!(remainder.len() <= 1, "partial frame must not be retried");
}

#[tokio::test(start_paused = true)]
async fn disconnect_interrupts_partial_write_before_deadline() {
    let mut peer = Peer::start().await;
    peer.block_write().await;
    let start = tokio::time::Instant::now();
    peer.cancel
        .send_replace(Some(DisconnectReason::UserInitiated));
    assert!(matches!(
        peer.task.await.unwrap(),
        DisconnectReason::UserInitiated
    ));
    assert!(start.elapsed() < IO_TIMEOUT);
    let mut remainder = Vec::new();
    peer.socket.read_to_end(&mut remainder).await.unwrap();
    assert!(remainder.len() <= 1);
}

#[tokio::test(start_paused = true)]
async fn incomplete_reactivation_has_one_deadline() {
    let mut peer = Peer::start().await;
    peer.deactivate().await;
    assert!(
        matches!(peer.task.await.unwrap(), DisconnectReason::ConnectionLost(ref text)
        if text == "Timed out reactivating the session")
    );
}

#[tokio::test(start_paused = true)]
async fn disconnect_interrupts_incomplete_reactivation() {
    let mut peer = Peer::start().await;
    peer.deactivate().await;
    let start = tokio::time::Instant::now();
    peer.cancel
        .send_replace(Some(DisconnectReason::UserInitiated));
    assert!(matches!(
        peer.task.await.unwrap(),
        DisconnectReason::UserInitiated
    ));
    assert!(start.elapsed() < ACTIVATION_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn dropping_ui_interrupts_blocked_transport() {
    let mut peer = Peer::start().await;
    peer.block_write().await;
    let start = tokio::time::Instant::now();
    drop(peer.events);
    assert!(matches!(
        peer.task.await.unwrap(),
        DisconnectReason::UserInitiated
    ));
    assert!(start.elapsed() < IO_TIMEOUT);
}
