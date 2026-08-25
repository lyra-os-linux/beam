//! The active-session loop: reads PDUs from the network, decodes them into the framebuffer,
//! and turns local commands (input, clipboard, Ctrl+Alt+Del, disconnect) into outgoing PDUs.

use std::sync::Arc;
use std::time::Duration;

use ironrdp_cliprdr::pdu::{ClipboardFormat, ClipboardFormatId};
use ironrdp_cliprdr::CliprdrClient;
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::ConnectionResult;
use ironrdp_core::{IntoOwned as _, WriteBuf};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::input::fast_path::FastPathInputEvent;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{fast_path, ActiveStageBuilder, ActiveStageOutput, GracefulDisconnectReason};
use ironrdp_tls::TlsStream;
use ironrdp_tokio::{single_sequence_step_read, split_tokio_framed, Framed, FramedWrite as _};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tracing::{error, warn};

use crate::events::{DisconnectReason, SessionEvent};
use crate::session::clipboard::ClipboardBackendMsg;
use crate::session::framebuffer::{DirtyRect, Framebuffer};
use crate::session::input;
use crate::session::SessionCommand;

fn to_dirty_rect(region: &ironrdp_pdu::geometry::InclusiveRectangle) -> DirtyRect {
    DirtyRect {
        x: region.left,
        y: region.top,
        width: region.right.saturating_sub(region.left).saturating_add(1),
        height: region.bottom.saturating_sub(region.top).saturating_add(1),
    }
}

// This is the single session orchestration boundary: keeping the independently
// bounded command/event channels explicit makes backpressure policy reviewable.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    framed: Framed<ironrdp_tokio::TokioStream<TlsStream<TcpStream>>>,
    connection_result: ConnectionResult,
    events: mpsc::Sender<SessionEvent>,
    mut commands: mpsc::Receiver<SessionCommand>,
    mut essential_commands: mpsc::Receiver<SessionCommand>,
    mut pointer_position: watch::Receiver<Option<crate::session::InputEvent>>,
    mut clipboard_generation: watch::Receiver<u64>,
    mut cancelled: watch::Receiver<bool>,
    mut clipboard_backend_rx: mpsc::UnboundedReceiver<ClipboardBackendMsg>,
    framebuffer: Arc<Framebuffer>,
) {
    let (mut reader, mut writer) = split_tokio_framed(framed);

    let desktop_size = connection_result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::BgrA32, desktop_size.width, desktop_size.height);
    framebuffer.replace(
        desktop_size.width,
        desktop_size.height,
        image.data().to_vec(),
    );

    let _ = events
        .send(SessionEvent::Connected {
            width: desktop_size.width,
            height: desktop_size.height,
        })
        .await;

    let activation_factory = connection_result.activation_factory;

    let mut active_stage = ActiveStageBuilder {
        static_channels: connection_result.static_channels,
        user_channel_id: connection_result.user_channel_id,
        io_channel_id: connection_result.io_channel_id,
        message_channel_id: connection_result.message_channel_id,
        share_id: connection_result.share_id,
        compression_type: connection_result.compression_type,
        enable_server_pointer: connection_result.enable_server_pointer,
        pointer_software_rendering: connection_result.pointer_software_rendering,
    }
    .build();

    let mut cleanup_interval = tokio::time::interval(Duration::from_secs(5));

    let disconnect_reason = 'outer: loop {
        let outputs = tokio::select! {
            _ = cancelled.changed() => {
                break 'outer GracefulDisconnectReason::UserInitiated;
            }
            frame = reader.read_pdu() => {
                let (action, payload) = match frame {
                    Ok(frame) => frame,
                    Err(e) => {
                        let _ = events.send(SessionEvent::Disconnected(DisconnectReason::ConnectionLost(e.to_string()))).await;
                        return;
                    }
                };
                match active_stage.process(&mut image, action, &payload) {
                    Ok(outputs) => outputs,
                    Err(e) => {
                        let _ = events.send(SessionEvent::Disconnected(DisconnectReason::ConnectionLost(e.report().to_string()))).await;
                        return;
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    // Frontend dropped the handle: treat as a local disconnect request.
                    break 'outer GracefulDisconnectReason::UserInitiated;
                };
                match command {
                    SessionCommand::Input(event) => {
                        let fastpath_events = input::to_fastpath(event);
                        process_input(&mut active_stage, &mut image, &fastpath_events)
                    }
                    SessionCommand::CtrlAltDel => {
                        let fastpath_events = input::ctrl_alt_del_sequence();
                        process_input(&mut active_stage, &mut image, &fastpath_events)
                    }
                    SessionCommand::Disconnect => {
                        match active_stage.graceful_shutdown() {
                            Ok(outputs) => outputs,
                            Err(_) => break 'outer GracefulDisconnectReason::UserInitiated,
                        }
                    }
                }
            }
            command = essential_commands.recv() => {
                match command {
                    Some(SessionCommand::Input(event)) => {
                        let fastpath_events = input::to_fastpath(event);
                        process_input(&mut active_stage, &mut image, &fastpath_events)
                    }
                    Some(SessionCommand::CtrlAltDel) => {
                        let fastpath_events = input::ctrl_alt_del_sequence();
                        process_input(&mut active_stage, &mut image, &fastpath_events)
                    }
                    _ => Vec::new(),
                }
            }
            changed = clipboard_generation.changed() => {
                if changed.is_err() {
                    Vec::new()
                } else {
                    clipboard_generation.borrow_and_update();
                    with_cliprdr(&mut active_stage, &events, |cliprdr| {
                        cliprdr.initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
                    })
                }
            }
            changed = pointer_position.changed() => {
                if changed.is_err() {
                    Vec::new()
                } else if let Some(event) = *pointer_position.borrow_and_update() {
                    let fastpath_events = input::to_fastpath(event);
                    process_input(&mut active_stage, &mut image, &fastpath_events)
                } else {
                    Vec::new()
                }
            }
            clipboard_msg = clipboard_backend_rx.recv() => {
                match clipboard_msg {
                    None => Vec::new(),
                    Some(ClipboardBackendMsg::InitiateCopy) => {
                        with_cliprdr(&mut active_stage, &events, |cliprdr| {
                            cliprdr.initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
                        })
                    }
                    Some(ClipboardBackendMsg::InitiatePaste) => {
                        with_cliprdr(&mut active_stage, &events, |cliprdr| {
                            cliprdr.initiate_paste(ClipboardFormatId::CF_UNICODETEXT)
                        })
                    }
                    Some(ClipboardBackendMsg::SendFormatData(bytes)) => {
                        with_cliprdr(&mut active_stage, &events, |cliprdr| {
                            cliprdr.submit_format_data(
                                ironrdp_cliprdr::pdu::FormatDataResponse::new_data(bytes).into_owned(),
                            )
                        })
                    }
                    Some(ClipboardBackendMsg::TextReceived(text)) => {
                        let _ = events.send(SessionEvent::ClipboardTextReceived(text)).await;
                        Vec::new()
                    }
                }
            }
            _ = cleanup_interval.tick() => {
                with_cliprdr(&mut active_stage, &events, |cliprdr| cliprdr.drive_timeouts())
            }
        };

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if writer.write_all(&frame).await.is_err() {
                        let _ = events
                            .send(SessionEvent::Disconnected(
                                DisconnectReason::ConnectionLost(
                                    "falha ao enviar dados".to_owned(),
                                ),
                            ))
                            .await;
                        return;
                    }
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    let dirty = to_dirty_rect(&region);
                    framebuffer.update_region(image.data(), image.width(), image.height(), dirty);
                    let _ = events.try_send(SessionEvent::FramebufferUpdated { rect: dirty });
                }
                ActiveStageOutput::PointerDefault
                | ActiveStageOutput::PointerHidden
                | ActiveStageOutput::PointerPosition { .. }
                | ActiveStageOutput::PointerBitmap(_) => {
                    // v1 always shows the local cursor; remote pointer shapes are not rendered
                    // (documented limitation, see README).
                }
                ActiveStageOutput::DeactivateAll => {
                    let mut connection_activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = match single_sequence_step_read(
                            &mut reader,
                            &mut connection_activation,
                            &mut buf,
                        )
                        .await
                        {
                            Ok(written) => written,
                            Err(e) => {
                                let _ = events
                                    .send(SessionEvent::Disconnected(
                                        DisconnectReason::ConnectionLost(e.report().to_string()),
                                    ))
                                    .await;
                                return;
                            }
                        };
                        if written.size().is_some() && writer.write_all(buf.filled()).await.is_err()
                        {
                            let _ = events
                                .send(SessionEvent::Disconnected(
                                    DisconnectReason::ConnectionLost(
                                        "falha ao enviar dados".to_owned(),
                                    ),
                                ))
                                .await;
                            return;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                        } = connection_activation.connection_activation_state()
                        {
                            image = DecodedImage::new(
                                PixelFormat::BgrA32,
                                desktop_size.width,
                                desktop_size.height,
                            );
                            active_stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: connection_activation.io_channel_id(),
                                    user_channel_id: connection_activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    bulk_decompressor: None,
                                }
                                .build(),
                            );
                            active_stage.set_share_id(share_id);
                            active_stage.set_enable_server_pointer(enable_server_pointer);
                            framebuffer.replace(
                                desktop_size.width,
                                desktop_size.height,
                                image.data().to_vec(),
                            );
                            let _ = events
                                .send(SessionEvent::Connected {
                                    width: desktop_size.width,
                                    height: desktop_size.height,
                                })
                                .await;
                            break;
                        }
                    }
                }
                ActiveStageOutput::MultitransportRequest(_) | ActiveStageOutput::AutoDetect(_) => {}
                ActiveStageOutput::Terminate(reason) => break 'outer reason,
            }
        }
    };

    let reason = match disconnect_reason {
        GracefulDisconnectReason::UserInitiated => DisconnectReason::UserInitiated,
        other => DisconnectReason::ConnectionLost(other.to_string()),
    };
    let _ = events.send(SessionEvent::Disconnected(reason)).await;
}

fn process_input(
    active_stage: &mut ironrdp_session::ActiveStage,
    image: &mut DecodedImage,
    events: &[FastPathInputEvent],
) -> Vec<ActiveStageOutput> {
    match active_stage.process_fastpath_input(image, events) {
        Ok(outputs) => outputs,
        Err(error) => {
            error!(error = %error.report(), "falha ao codificar entrada fast-path");
            Vec::new()
        }
    }
}

/// Runs `f` against the active CLIPRDR processor, if the channel is available, flattening any
/// PDU-encoding error into an empty output (logged, not fatal to the session).
fn with_cliprdr(
    active_stage: &mut ironrdp_session::ActiveStage,
    events: &mpsc::Sender<SessionEvent>,
    f: impl FnOnce(
        &mut CliprdrClient,
    ) -> ironrdp_pdu::PduResult<
        ironrdp_cliprdr::CliprdrSvcMessages<ironrdp_cliprdr::Client>,
    >,
) -> Vec<ActiveStageOutput> {
    let Some(cliprdr) = active_stage.get_svc_processor_mut::<CliprdrClient>() else {
        warn!("canal de clipboard indisponível");
        notify_clipboard_failure(events, "canal CLIPRDR não foi negociado");
        return Vec::new();
    };
    let messages = match f(cliprdr) {
        Ok(messages) => messages,
        Err(error) => {
            error!(%error, "falha ao gerar mensagem CLIPRDR");
            notify_clipboard_failure(events, &format!("falha ao gerar mensagem: {error}"));
            return Vec::new();
        }
    };
    match active_stage.process_svc_processor_messages(messages) {
        Ok(frame) if !frame.is_empty() => vec![ActiveStageOutput::ResponseFrame(frame)],
        Ok(_) => Vec::new(),
        Err(error) => {
            error!(error = %error.report(), "falha ao processar mensagem CLIPRDR");
            notify_clipboard_failure(
                events,
                &format!("falha ao processar mensagem: {}", error.report()),
            );
            Vec::new()
        }
    }
}

fn notify_clipboard_failure(events: &mpsc::Sender<SessionEvent>, detail: &str) {
    let _ = events.try_send(SessionEvent::FeatureUnavailable {
        feature: "clipboard",
        detail: detail.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clipboard_failure_emits_actionable_diagnostic_without_sensitive_content() {
        let (events, mut receiver) = mpsc::channel(1);
        notify_clipboard_failure(&events, "canal não negociado");
        match receiver.recv().await {
            Some(SessionEvent::FeatureUnavailable { feature, detail }) => {
                assert_eq!(feature, "clipboard");
                assert_eq!(detail, "canal não negociado");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
