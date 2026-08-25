//! Bidirectional text clipboard backend (CF_UNICODETEXT only — no file support, per v1 scope).
//!
//! IronRDP's [`CliprdrBackend`] trait is synchronous and is driven from the tokio task running
//! the active session; it must never touch GTK/GDK objects, which are thread-affine to the GTK
//! main loop. So this backend only ever *reads* an already-resolved local-clipboard cache
//! ([`ClipboardBridge::set_local_text`], written by the frontend whenever the GTK clipboard
//! changes) and *writes* out notifications ([`ClipboardBackendMsg`]) for the active-session loop
//! to act on. Nothing here blocks waiting on the GTK thread.

use std::sync::Mutex;

use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
};
use tokio::sync::mpsc;

/// Shared cache of "what's currently on the local clipboard", written by the frontend and read
/// synchronously by [`BeamClipboardBackend::on_format_data_request`].
#[derive(Default)]
pub struct ClipboardBridge {
    local_text: Mutex<Option<String>>,
}

impl ClipboardBridge {
    pub fn set_local_text(&self, text: String) {
        *self
            .local_text
            .lock()
            .expect("clipboard bridge mutex poisoned") = Some(text);
    }

    fn get_local_text(&self) -> Option<String> {
        self.local_text
            .lock()
            .expect("clipboard bridge mutex poisoned")
            .clone()
    }
}

/// Messages produced by the clipboard backend for the active-session loop to act on.
pub(crate) enum ClipboardBackendMsg {
    /// The local clipboard has text ready; advertise it to the remote.
    InitiateCopy,
    /// The remote clipboard changed and offers text; go fetch it.
    InitiatePaste,
    /// The remote asked for our clipboard data; here it is, ready to submit.
    SendFormatData(Vec<u8>),
    /// Text arrived from the remote clipboard; forward it up to the frontend.
    TextReceived(String),
}

fn text_to_cf_unicodetext(text: &str) -> Vec<u8> {
    // CF_UNICODETEXT is UTF-16LE, NUL-terminated. Windows-side consumers expect CRLF line endings.
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let mut buf = Vec::with_capacity(normalized.len() * 2 + 2);
    for unit in normalized.encode_utf16() {
        buf.extend_from_slice(&unit.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

// `slice::as_chunks` requires a newer compiler than Beam's supported MSRV.
#[allow(clippy::chunks_exact_to_as_chunks)]
fn cf_unicodetext_to_text(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end]).replace("\r\n", "\n")
}

struct BeamClipboardBackend {
    bridge: std::sync::Arc<ClipboardBridge>,
    tx: mpsc::UnboundedSender<ClipboardBackendMsg>,
}

ironrdp_core::impl_as_any!(BeamClipboardBackend);

impl std::fmt::Debug for BeamClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeamClipboardBackend")
            .finish_non_exhaustive()
    }
}

impl CliprdrBackend for BeamClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ""
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        if self.bridge.get_local_text().is_some() {
            let _ = self.tx.send(ClipboardBackendMsg::InitiateCopy);
        }
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        let has_text = available_formats.iter().any(|f| {
            f.id() == ClipboardFormatId::CF_UNICODETEXT || f.id() == ClipboardFormatId::CF_TEXT
        });
        if has_text {
            let _ = self.tx.send(ClipboardBackendMsg::InitiatePaste);
        }
    }

    fn on_format_data_request(&mut self, _request: FormatDataRequest) {
        let text = self.bridge.get_local_text().unwrap_or_default();
        let _ = self
            .tx
            .send(ClipboardBackendMsg::SendFormatData(text_to_cf_unicodetext(
                &text,
            )));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        let text = cf_unicodetext_to_text(response.data());
        let _ = self.tx.send(ClipboardBackendMsg::TextReceived(text));
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {
        // File transfer is out of v1 scope; silently ignore.
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

pub(crate) fn build_backend(
    bridge: std::sync::Arc<ClipboardBridge>,
    tx: mpsc::UnboundedSender<ClipboardBackendMsg>,
) -> Box<dyn CliprdrBackend> {
    Box::new(BeamClipboardBackend { bridge, tx })
}
