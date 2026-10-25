//! Janelas de sessão. Cada janela `session-*` dirige uma sessão do beam-core:
//! os quadros vão para um `<canvas>` por um canal binário (só a região que
//! mudou, um quadro por vez com confirmação da interface), os pedidos de
//! certificado e senha viram diálogos na própria janela e a entrada volta
//! como `InputEvent`s. Nenhum tipo do IronRDP atravessa esta camada.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use beam_core::events::{DisconnectReason, SessionEvent};
use beam_core::known_hosts::TrustDecision;
use beam_core::profile::ConnectionProfile;
use beam_core::session::framebuffer::DirtyRect;
use beam_core::session::{
    self, Framebuffer, InputEvent, PointerButton, SessionController, SessionEvents,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::{oneshot, Notify};
use uuid::Uuid;

use crate::clipboard;
use crate::i18n::gettext;
use crate::manager::{self, Profiles};

enum Prompt {
    Cert(oneshot::Sender<bool>),
    Creds(oneshot::Sender<Option<(String, bool)>>),
}

/// Quadros pendentes de uma janela: a união das regiões alteradas desde o
/// último envio, liberada só depois que a interface confirmou o anterior.
struct FramePipe {
    channel: Channel<InvokeResponseBody>,
    framebuffer: Mutex<Option<Arc<Framebuffer>>>,
    dirty: Mutex<Option<DirtyRect>>,
    ready: AtomicBool,
    closed: AtomicBool,
    wake: Notify,
}

impl FramePipe {
    fn mark(&self, rect: DirtyRect) {
        let mut dirty = self.dirty.lock().unwrap();
        *dirty = Some(match *dirty {
            Some(current) => union(current, rect),
            None => rect,
        });
        drop(dirty);
        self.wake.notify_one();
    }

    fn mark_all(&self) {
        let size = self
            .framebuffer
            .lock()
            .unwrap()
            .as_ref()
            .map(|fb| fb.metadata());
        if let Some((width, height, _)) = size {
            self.mark(DirtyRect {
                x: 0,
                y: 0,
                width,
                height,
            });
        }
    }

    /// Envia a região pendente se a interface estiver pronta. Cabeçalho de
    /// 12 bytes (largura e altura do desktop, x, y, largura e altura da
    /// região, u16 little-endian) seguido dos pixels RGBA.
    fn flush(&self) {
        if self.closed.load(Ordering::Acquire) || !self.ready.load(Ordering::Acquire) {
            return;
        }
        let Some(fb) = self.framebuffer.lock().unwrap().clone() else {
            return;
        };
        let Some(rect) = self.dirty.lock().unwrap().take() else {
            return;
        };
        let (width, height, _) = fb.metadata();
        let Some((rect, pixels)) = fb.copy_region_rgba(rect) else {
            return;
        };
        let mut message = Vec::with_capacity(12 + pixels.len());
        for value in [width, height, rect.x, rect.y, rect.width, rect.height] {
            message.extend_from_slice(&value.to_le_bytes());
        }
        message.extend_from_slice(&pixels);
        self.ready.store(false, Ordering::Release);
        if self.channel.send(InvokeResponseBody::Raw(message)).is_err() {
            self.closed.store(true, Ordering::Release);
        }
    }
}

fn union(a: DirtyRect, b: DirtyRect) -> DirtyRect {
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = (u32::from(a.x) + u32::from(a.width)).max(u32::from(b.x) + u32::from(b.width));
    let bottom = (u32::from(a.y) + u32::from(a.height)).max(u32::from(b.y) + u32::from(b.height));
    DirtyRect {
        x: left,
        y: top,
        width: (right - u32::from(left)).min(u32::from(u16::MAX)) as u16,
        height: (bottom - u32::from(top)).min(u32::from(u16::MAX)) as u16,
    }
}

struct Live {
    profile: ConnectionProfile,
    /// Cada reconexão ganha uma geração; eventos de uma sessão antiga são ignorados.
    generation: u64,
    controller: Option<SessionController>,
    pipe: Option<Arc<FramePipe>>,
    prompts: HashMap<u64, Prompt>,
    state: &'static str,
}

pub struct Sessions {
    runtime: tokio::runtime::Handle,
    live: Mutex<HashMap<String, Live>>,
    next_prompt: AtomicU64,
}

impl Sessions {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            live: Mutex::new(HashMap::new()),
            next_prompt: AtomicU64::new(1),
        }
    }

    fn controller(&self, label: &str) -> Option<SessionController> {
        self.live.lock().unwrap().get(label)?.controller.clone()
    }

    /// Texto colado no sistema: oferecido a todas as sessões conectadas.
    pub fn offer_clipboard(&self, text: &str) {
        for live in self.live.lock().unwrap().values() {
            if let (Some(controller), "connected") = (&live.controller, live.state) {
                controller.set_local_clipboard_text(text.to_owned());
            }
        }
    }

    /// A janela foi destruída: encerra a sessão e esquece o estado.
    pub fn window_closed(&self, app: &AppHandle, label: &str) {
        let removed = self.live.lock().unwrap().remove(label);
        if let Some(live) = removed {
            if let Some(controller) = live.controller {
                controller.disconnect();
            }
            if let Some(pipe) = live.pipe {
                pipe.closed.store(true, Ordering::Release);
                pipe.wake.notify_one();
            }
            notify_manager(app);
        }
    }
}

#[derive(Serialize, Clone)]
pub struct SessionSummary {
    profile_id: Uuid,
    state: &'static str,
}

fn notify_manager(app: &AppHandle) {
    let _ = app.emit_to("main", "sessions-changed", sessions_snapshot(app));
}

fn sessions_snapshot(app: &AppHandle) -> Vec<SessionSummary> {
    app.state::<Sessions>()
        .live
        .lock()
        .unwrap()
        .values()
        .map(|live| SessionSummary {
            profile_id: live.profile.id,
            state: live.state,
        })
        .collect()
}

#[tauri::command]
pub fn sessions_state(app: AppHandle) -> Vec<SessionSummary> {
    sessions_snapshot(&app)
}

/// "Conectar" no gerenciador: reapresenta a janela da conexão, se já
/// existir, ou abre uma nova janela de sessão.
#[tauri::command]
pub async fn connect(
    app: AppHandle,
    profiles: tauri::State<'_, Profiles>,
    id: Uuid,
) -> Result<(), String> {
    let profile = manager::find(&profiles, id).ok_or_else(|| gettext("Connection not found."))?;
    let existing = {
        let sessions = app.state::<Sessions>();
        let live = sessions.live.lock().unwrap();
        live.iter()
            .find(|(_, l)| l.profile.id == id)
            .map(|(label, _)| label.clone())
    };
    if let Some(window) = existing.and_then(|label| app.get_webview_window(&label)) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let label = format!("session-{}", Uuid::new_v4().simple());
    app.state::<Sessions>().live.lock().unwrap().insert(
        label.clone(),
        Live {
            profile: profile.clone(),
            generation: 0,
            controller: None,
            pipe: None,
            prompts: HashMap::new(),
            state: "connecting",
        },
    );
    let (width, height) = window_size(&profile);
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("session.html".into()))
        .title(format!("{} — Beam", profile.name))
        .inner_size(width, height)
        .min_inner_size(480.0, 360.0)
        .decorations(false)
        .center()
        .build()
        .map_err(|error| error.to_string())?;
    notify_manager(&app);
    Ok(())
}

/// Tamanho inicial: a resolução da conexão com espaço para a barra, limitado
/// a uma janela confortável; a imagem remota é sempre escalada para caber.
fn window_size(profile: &ConnectionProfile) -> (f64, f64) {
    let width = f64::from(profile.resolution.width).min(1280.0);
    let height = f64::from(profile.resolution.height).min(800.0) + 46.0;
    (width, height)
}

/// Ganchos de desenvolvimento, só em builds de debug, para validar uma sessão
/// de ponta a ponta sem cliques: `BEAM_DEV_CONNECT=<id do perfil>` conecta ao
/// abrir, `BEAM_DEV_TRUST=1` aceita o certificado, `BEAM_DEV_PASSWORD` responde
/// a senha (sem salvar no chaveiro) e `BEAM_DEV_SNAPSHOT=<arquivo.png>` grava o
/// canvas depois de conectar; `BEAM_DEV_TYPE` digita um texto pelo mesmo
/// caminho do teclado (\\n vira Enter).
pub mod dev {
    fn var(name: &str) -> Option<String> {
        cfg!(debug_assertions)
            .then(|| std::env::var(name).ok())
            .flatten()
            .filter(|v| !v.is_empty())
    }
    pub fn trust() -> bool {
        var("BEAM_DEV_TRUST").as_deref() == Some("1")
    }
    pub fn password() -> Option<String> {
        var("BEAM_DEV_PASSWORD")
    }

    #[tauri::command]
    pub fn dev_autoconnect() -> Option<String> {
        var("BEAM_DEV_CONNECT")
    }

    #[tauri::command]
    pub fn dev_snapshot_path() -> Option<String> {
        var("BEAM_DEV_SNAPSHOT")
    }

    /// Texto que a janela de sessão digita depois de conectar (`BEAM_DEV_TYPE`).
    #[tauri::command]
    pub fn dev_type_text() -> Option<String> {
        var("BEAM_DEV_TYPE")
    }

    /// Grava também o framebuffer do beam-core (PPM), antes do transporte.
    #[tauri::command]
    pub fn dev_dump_framebuffer(
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
    ) -> Result<(), String> {
        use tauri::Manager;
        let path = var("BEAM_DEV_SNAPSHOT").ok_or("BEAM_DEV_SNAPSHOT not set")?;
        let sessions = app.state::<super::Sessions>();
        let fb = sessions
            .live
            .lock()
            .unwrap()
            .get(window.label())
            .and_then(|l| l.pipe.as_ref()?.framebuffer.lock().unwrap().clone())
            .ok_or("no framebuffer")?;
        let (width, height, bgra) = fb.snapshot();
        let mut ppm = format!("P6 {width} {height} 255\n").into_bytes();
        for px in bgra.chunks(4) {
            ppm.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        std::fs::write(format!("{path}.fb.ppm"), ppm).map_err(|e| e.to_string())
    }

    /// Grava o PNG (data URL) do canvas no caminho de `BEAM_DEV_SNAPSHOT`.
    #[tauri::command]
    pub fn dev_snapshot(data_url: String) -> Result<(), String> {
        use base64::Engine;
        let path = var("BEAM_DEV_SNAPSHOT").ok_or("BEAM_DEV_SNAPSHOT not set")?;
        let encoded = data_url
            .split_once(',')
            .map(|(_, b)| b)
            .ok_or("invalid data URL")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}

#[derive(Serialize)]
pub struct SessionInfo {
    name: String,
    host: String,
    port: u16,
    width: u16,
    height: u16,
    color_depth: u32,
    fullscreen: bool,
}

/// Chamado pela janela de sessão ao carregar: registra o canal de quadros e
/// inicia a conexão.
#[tauri::command]
pub fn session_open(
    app: AppHandle,
    window: tauri::WebviewWindow,
    frames: Channel<InvokeResponseBody>,
) -> Result<SessionInfo, String> {
    let label = window.label().to_owned();
    let pipe = Arc::new(FramePipe {
        channel: frames,
        framebuffer: Mutex::new(None),
        dirty: Mutex::new(None),
        ready: AtomicBool::new(true),
        closed: AtomicBool::new(false),
        wake: Notify::new(),
    });
    let profile = {
        let sessions = app.state::<Sessions>();
        let mut live = sessions.live.lock().unwrap();
        let entry = live
            .get_mut(&label)
            .ok_or_else(|| gettext("Connection not found."))?;
        if let Some(old) = entry.pipe.replace(pipe.clone()) {
            old.closed.store(true, Ordering::Release);
            old.wake.notify_one();
        }
        entry.profile.clone()
    };
    let runtime = app.state::<Sessions>().runtime.clone();
    runtime.spawn(frame_loop(pipe));
    start(&app, &label);
    Ok(SessionInfo {
        name: profile.name,
        host: profile.host,
        port: profile.port,
        width: profile.resolution.width,
        height: profile.resolution.height,
        color_depth: profile.color_depth,
        fullscreen: profile.fullscreen,
    })
}

async fn frame_loop(pipe: Arc<FramePipe>) {
    while !pipe.closed.load(Ordering::Acquire) {
        pipe.wake.notified().await;
        pipe.flush();
    }
}

fn start(app: &AppHandle, label: &str) {
    let sessions = app.state::<Sessions>();
    let (events, generation) = {
        let mut live = sessions.live.lock().unwrap();
        let Some(entry) = live.get_mut(label) else {
            return;
        };
        if let Some(old) = entry.controller.take() {
            old.disconnect();
        }
        let (controller, events) = session::connect(entry.profile.clone(), &sessions.runtime);
        if let Some(pipe) = &entry.pipe {
            *pipe.framebuffer.lock().unwrap() = Some(controller.framebuffer().clone());
            *pipe.dirty.lock().unwrap() = None;
        }
        entry.generation += 1;
        entry.controller = Some(controller);
        entry.prompts.clear();
        entry.state = "connecting";
        (events, entry.generation)
    };
    notify_manager(app);
    sessions
        .runtime
        .spawn(pump(app.clone(), label.to_owned(), generation, events));
}

fn emit(app: &AppHandle, label: &str, payload: serde_json::Value) {
    let _ = app.emit_to(label, "session-event", payload);
}

async fn pump(app: AppHandle, label: String, generation: u64, mut events: SessionEvents) {
    while let Some(event) = events.next_event().await {
        let sessions = app.state::<Sessions>();
        let pipe = {
            let live = sessions.live.lock().unwrap();
            match live.get(&label) {
                Some(entry) if entry.generation == generation => entry.pipe.clone(),
                _ => return,
            }
        };
        match event {
            SessionEvent::Connected { width, height } => {
                set_state(&app, &label, "connected");
                if let Some(pipe) = &pipe {
                    pipe.mark_all();
                }
                emit(
                    &app,
                    &label,
                    json!({ "kind": "connected", "width": width, "height": height }),
                );
            }
            SessionEvent::FramebufferUpdated { rect } => {
                if let Some(pipe) = &pipe {
                    pipe.mark(rect);
                }
            }
            SessionEvent::CertPrompt(request) if dev::trust() => {
                let _ = request.respond.send(true);
            }
            SessionEvent::CredsNeeded(request) if dev::password().is_some() => {
                let _ = request
                    .respond
                    .send(dev::password().map(|password| (password, false)));
            }
            SessionEvent::CertPrompt(request) => {
                let id = sessions.next_prompt.fetch_add(1, Ordering::Relaxed);
                let (decision, previous) = match &request.decision {
                    TrustDecision::Mismatch { previous } => {
                        ("changed", Some(previous.as_str().to_owned()))
                    }
                    TrustDecision::FirstUse => ("first", None),
                    TrustDecision::Trusted => ("trusted", None),
                };
                remember(&app, &label, id, Prompt::Cert(request.respond));
                emit(
                    &app,
                    &label,
                    json!({
                        "kind": "cert", "id": id, "address": request.address,
                        "fingerprint": request.fingerprint.as_str(), "decision": decision, "previous": previous,
                    }),
                );
            }
            SessionEvent::CredsNeeded(request) => {
                let id = sessions.next_prompt.fetch_add(1, Ordering::Relaxed);
                remember(&app, &label, id, Prompt::Creds(request.respond));
                emit(
                    &app,
                    &label,
                    json!({ "kind": "creds", "id": id, "username": request.username }),
                );
            }
            SessionEvent::ClipboardTextReceived(text) => clipboard::set_text(&app, text),
            SessionEvent::FeatureUnavailable { feature, detail } => {
                let text = gettext("{feature} unavailable: {detail}")
                    .replace("{feature}", feature)
                    .replace("{detail}", &detail);
                emit(&app, &label, json!({ "kind": "notice", "text": text }));
            }
            SessionEvent::Disconnected(reason) => {
                match reason {
                    DisconnectReason::UserInitiated => {
                        if let Some(window) = app.get_webview_window(&label) {
                            let _ = window.destroy();
                        }
                    }
                    DisconnectReason::ConnectionLost(detail) => {
                        set_state(&app, &label, "lost");
                        let text = gettext("Connection lost: {detail}")
                            .replace("{detail}", &localized_detail(&detail));
                        emit(&app, &label, json!({ "kind": "lost", "text": text }));
                    }
                    DisconnectReason::ConnectionFailed(detail) => {
                        set_state(&app, &label, "lost");
                        let text = gettext("Connection failed: {detail}")
                            .replace("{detail}", &localized_detail(&detail));
                        emit(&app, &label, json!({ "kind": "failed", "text": text }));
                    }
                }
                break;
            }
        }
    }
}

fn set_state(app: &AppHandle, label: &str, state: &'static str) {
    if let Some(entry) = app.state::<Sessions>().live.lock().unwrap().get_mut(label) {
        entry.state = state;
        if state != "connecting" {
            entry.prompts.clear();
        }
    }
    notify_manager(app);
}

fn remember(app: &AppHandle, label: &str, id: u64, prompt: Prompt) {
    if let Some(entry) = app.state::<Sessions>().live.lock().unwrap().get_mut(label) {
        entry.prompts.insert(id, prompt);
    }
}

fn take_prompt(app: &AppHandle, label: &str, id: u64) -> Option<Prompt> {
    app.state::<Sessions>()
        .live
        .lock()
        .unwrap()
        .get_mut(label)?
        .prompts
        .remove(&id)
}

/// Traduz os textos do próprio Beam e mantém os detalhes técnicos de rede como vieram.
fn localized_detail(detail: &str) -> String {
    match detail {
        "Input queue full; disconnected to avoid losing key releases" => {
            gettext("Input queue full; disconnected to avoid losing key releases")
        }
        "Timed out sending data" => gettext("Timed out sending data"),
        "Timed out reactivating the session" => gettext("Timed out reactivating the session"),
        "Session ended unexpectedly" => gettext("Session ended unexpectedly"),
        "falha ao enviar dados" => gettext("Failed to send data"),
        "certificado do servidor recusado pelo usuário" => {
            gettext("Server certificate rejected by the user")
        }
        "conexão cancelada pelo usuário" => gettext("Connection cancelled by the user"),
        _ if detail.starts_with("falha de rede: ") => gettext("Network error: {detail}")
            .replace("{detail}", detail.trim_start_matches("falha de rede: ")),
        _ => detail.to_owned(),
    }
}

#[tauri::command]
pub fn session_answer_cert(app: AppHandle, window: tauri::WebviewWindow, id: u64, accept: bool) {
    if let Some(Prompt::Cert(respond)) = take_prompt(&app, window.label(), id) {
        let _ = respond.send(accept);
    }
}

#[tauri::command]
pub fn session_answer_creds(
    app: AppHandle,
    window: tauri::WebviewWindow,
    id: u64,
    password: Option<String>,
    save: bool,
) {
    if let Some(Prompt::Creds(respond)) = take_prompt(&app, window.label(), id) {
        let _ = respond.send(password.map(|password| (password, save)));
    }
}

#[tauri::command]
pub fn session_frame_ack(app: AppHandle, window: tauri::WebviewWindow) {
    let pipe = app
        .state::<Sessions>()
        .live
        .lock()
        .unwrap()
        .get(window.label())
        .and_then(|l| l.pipe.clone());
    if let Some(pipe) = pipe {
        pipe.ready.store(true, Ordering::Release);
        pipe.wake.notify_one();
    }
}

/// Entrada já em coordenadas do desktop remoto e em scancodes PS/2 Set 1.
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Input {
    Move {
        x: u16,
        y: u16,
    },
    Button {
        x: u16,
        y: u16,
        button: u8,
        pressed: bool,
    },
    Wheel {
        x: u16,
        y: u16,
        steps: i16,
    },
    Key {
        scancode: u8,
        extended: bool,
        pressed: bool,
    },
}

impl Input {
    fn into_event(self) -> Option<InputEvent> {
        Some(match self {
            Input::Move { x, y } => InputEvent::MouseMove { x, y },
            Input::Button {
                x,
                y,
                button,
                pressed,
            } => InputEvent::MouseButton {
                x,
                y,
                button: match button {
                    0 => PointerButton::Left,
                    1 => PointerButton::Middle,
                    2 => PointerButton::Right,
                    _ => return None,
                },
                pressed,
            },
            Input::Wheel { x, y, steps } => InputEvent::MouseWheel { x, y, steps },
            Input::Key {
                scancode,
                extended,
                pressed,
            } => InputEvent::Key {
                scancode,
                extended,
                pressed,
            },
        })
    }
}

#[tauri::command]
pub fn session_input(app: AppHandle, window: tauri::WebviewWindow, events: Vec<Input>) {
    let Some(controller) = app.state::<Sessions>().controller(window.label()) else {
        return;
    };
    for event in events.into_iter().filter_map(Input::into_event) {
        controller.send_input(event);
    }
}

#[tauri::command]
pub fn session_ctrl_alt_del(app: AppHandle, window: tauri::WebviewWindow) {
    if let Some(controller) = app.state::<Sessions>().controller(window.label()) {
        controller.send_ctrl_alt_del();
    }
}

/// Desconectar: a sessão termina como iniciada pelo usuário e a janela fecha.
/// Sem sessão ativa (falhou ou caiu), fecha a janela direto.
#[tauri::command]
pub fn session_disconnect(app: AppHandle, window: tauri::WebviewWindow) {
    let live_controller = {
        let sessions = app.state::<Sessions>();
        let live = sessions.live.lock().unwrap();
        live.get(window.label())
            .filter(|l| l.state != "lost")
            .and_then(|l| l.controller.clone())
    };
    match live_controller {
        Some(controller) => controller.disconnect(),
        None => {
            let _ = window.destroy();
        }
    }
}

#[tauri::command]
pub fn session_reconnect(app: AppHandle, window: tauri::WebviewWindow) {
    start(&app, window.label());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, width: u16, height: u16) -> DirtyRect {
        DirtyRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn union_covers_both_regions() {
        assert_eq!(union(r(10, 10, 5, 5), r(0, 20, 2, 2)), r(0, 10, 15, 12));
        assert_eq!(union(r(0, 0, 4, 4), r(1, 1, 1, 1)), r(0, 0, 4, 4));
    }

    #[test]
    fn input_maps_buttons_and_rejects_unknown_ones() {
        let left: Input =
            serde_json::from_str(r#"{"t":"button","x":1,"y":2,"button":0,"pressed":true}"#)
                .unwrap();
        assert_eq!(
            left.into_event(),
            Some(InputEvent::MouseButton {
                x: 1,
                y: 2,
                button: PointerButton::Left,
                pressed: true
            })
        );
        let right: Input =
            serde_json::from_str(r#"{"t":"button","x":1,"y":2,"button":2,"pressed":false}"#)
                .unwrap();
        assert!(matches!(
            right.into_event(),
            Some(InputEvent::MouseButton {
                button: PointerButton::Right,
                ..
            })
        ));
        let back: Input =
            serde_json::from_str(r#"{"t":"button","x":1,"y":2,"button":3,"pressed":true}"#)
                .unwrap();
        assert!(back.into_event().is_none());
        let key: Input =
            serde_json::from_str(r#"{"t":"key","scancode":30,"extended":false,"pressed":true}"#)
                .unwrap();
        assert_eq!(
            key.into_event(),
            Some(InputEvent::Key {
                scancode: 30,
                extended: false,
                pressed: true
            })
        );
    }

    #[test]
    fn localized_detail_keeps_third_party_text() {
        assert_eq!(localized_detail("TLS alert 42"), "TLS alert 42");
    }

    #[test]
    fn window_size_is_capped_for_large_desktops() {
        let mut p = ConnectionProfile::new("a", "h", "u");
        p.resolution.width = 2560;
        p.resolution.height = 1440;
        assert_eq!(window_size(&p), (1280.0, 846.0));
    }
}
