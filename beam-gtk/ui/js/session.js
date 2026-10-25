// Beam — janela de sessão. Os quadros chegam pelo canal binário (só a região
// alterada, em RGBA) e são pintados num <canvas>; teclado e mouse voltam ao
// Rust como InputEvents do beam-core. Ctrl+Alt+F12 libera a captura.

import { call, newChannel, on, win } from "./api.js";
import { loadCatalog, tr, trf } from "./i18n.js";
import { scancodeFor } from "./keymap.js";
import { registerIcons } from "./icons.js";
import { h, icon, clear, button, iconButton, toggle, toast, overlay } from "./ui.js";

registerIcons({
  certificate: '<path d="M14 15H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v8"/><path d="M7 7h10M7 11h4"/><circle cx="18" cy="17" r="3"/><path d="m16.5 19.5-1 3 2.5-1.2 2.5 1.2-1-3"/>',
  eye: '<path d="M2.06 12.35a1 1 0 0 1 0-.7C3.42 8.36 7.4 5 12 5s8.58 3.36 9.94 6.65a1 1 0 0 1 0 .7C20.58 15.64 16.6 19 12 19s-8.58-3.36-9.94-6.65"/><circle cx="12" cy="12" r="3"/>',
  "eye-off": '<path d="M10.73 5.08A10.43 10.43 0 0 1 12 5c4.6 0 8.58 3.36 9.94 6.65a1 1 0 0 1 0 .7 10.75 10.75 0 0 1-1.44 2.49"/><path d="M14.08 14.16a3 3 0 0 1-4.24-4.24"/><path d="M17.48 17.5A10.75 10.75 0 0 1 12 19c-4.6 0-8.58-3.36-9.94-6.65a1 1 0 0 1 0-.7 10.75 10.75 0 0 1 4.45-5.14"/><path d="m2 2 20 20"/>',
});

const $ = (id) => document.getElementById(id);
const canvas = $("remote");
const ctx = canvas.getContext("2d", { alpha: false });

let info = { name: "", host: "", port: 3389, width: 1366, height: 768, color_depth: 32, fullscreen: false };
let state = "connecting";
let captured = false;
let fullscreen = false;
const pressed = new Map(); // code → [scancode, extended]

// ---------- Quadros ----------

function fit() {
  const stage = $("stage").getBoundingClientRect();
  const scale = Math.min(stage.width / canvas.width, stage.height / canvas.height);
  canvas.style.width = `${Math.floor(canvas.width * scale)}px`;
  canvas.style.height = `${Math.floor(canvas.height * scale)}px`;
}
new ResizeObserver(fit).observe($("stage"));

function paint(message) {
  const buffer = message instanceof ArrayBuffer ? message : message?.buffer ?? new Uint8Array(message).buffer;
  const head = new DataView(buffer, 0, 12);
  const [width, height, x, y, w, hh] = [0, 2, 4, 6, 8, 10].map((o) => head.getUint16(o, true));
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
    fit();
  }
  ctx.putImageData(new ImageData(new Uint8ClampedArray(buffer, 12, w * hh * 4), w, hh), x, y);
  // O próximo quadro só sai depois que este foi apresentado.
  requestAnimationFrame(() => call("session_frame_ack").catch(() => {}));
}

// ---------- Barra e estados ----------

function renderBar() {
  $("dot").className = `status-dot ${state === "connected" ? "" : state === "connecting" ? "connecting" : "lost"}`.trim();
  $("meta").textContent = state === "connected"
    ? `${info.host}${info.port !== 3389 ? `:${info.port}` : ""} · ${canvas.width}×${canvas.height} · ${trf("{bits} bits", { bits: info.color_depth })}`
    : state === "connecting" ? trf("Connecting to {host}…", { host: info.host }) : tr("Connection lost");
  const live = state === "connected";
  clear($("actions"),
    button(tr("Ctrl+Alt+Del"), { size: "sm", icon: "keyboard", disabled: !live, title: tr("Send Ctrl+Alt+Del"), onClick: () => { call("session_ctrl_alt_del"); canvas.focus(); } }),
    iconButton(fullscreen ? "restore" : "maximize", fullscreen ? tr("Leave fullscreen") : tr("Fullscreen"), toggleFullscreen, "sm"),
    button(tr("Disconnect"), { size: "sm", kind: "danger-soft", icon: "power", onClick: () => call("session_disconnect") }));
  $("capture").hidden = !(live && captured);
  clear($("capture"), icon("keyboard", 14), h("span", { class: "label" }, tr("Keyboard and mouse captured")), h("kbd", {}, "Ctrl"), h("kbd", {}, "Alt"), h("kbd", {}, "F12"), tr("releases"));
}

function showOverlay(content) {
  clear($("overlay"), content);
  $("overlay").hidden = !content;
  canvas.classList.toggle("blur", Boolean(content));
}

function connectingCard() {
  return h("div", { class: "connect-card", role: "status" },
    h("h2", {}, trf("Connecting to {name}", { name: info.name })),
    h("p", { class: "row-flex" }, h("span", { class: "spinner" }), `${info.host}:${info.port}`),
    h("div", { class: "row-flex" }, button(tr("Cancel"), { size: "sm", onClick: () => call("session_disconnect") })));
}

function lostCard(kind, text) {
  return h("div", { class: "connect-card lost-card", role: "alert" },
    h("div", { class: "d-ic" }, icon("alert-circle", 26)),
    h("h2", {}, kind === "failed" ? tr("Could not connect") : tr("Connection lost")),
    h("p", {}, kind === "failed"
      ? trf("The connection to {host} could not be completed.", { host: info.host })
      : trf("The session with {host} was interrupted. Whatever was open on the remote computer is still there.", { host: info.host })),
    h("div", { class: "detail selectable" }, text),
    h("div", { class: "row-flex" },
      button(tr("Close"), { size: "sm", onClick: () => win.close() }),
      button(tr("Reconnect"), { size: "sm", kind: "primary", icon: "refresh", onClick: reconnect })));
}

function setState(next) {
  state = next;
  if (next !== "connected") release();
  renderBar();
}

function reconnect() {
  setState("connecting");
  showOverlay(connectingCard());
  call("session_reconnect").catch((error) => toast(error.message, { ic: "alert-circle" }));
}

// ---------- Certificado e senha ----------

const groups = (fingerprint) => h("div", { class: "fingerprint" },
  fingerprint.split(":").reduce((acc, byte, i) => { if (i % 2 === 0) acc.push(byte); else acc[acc.length - 1] += byte; return acc; }, []).map((g) => h("span", {}, g)));

function certificateDialog(event) {
  let accepted = false;
  const answer = () => call("session_answer_cert", { id: event.id, accept: accepted });
  if (event.decision === "changed") {
    const cancel = h("button", { class: "btn primary", onclick: () => close() }, tr("Cancel connection"));
    const close = overlay(h("div", { class: "dialog", role: "alertdialog", "aria-label": tr("⚠ Possible attack detected") },
      h("div", { class: "dialog-body" },
        h("div", { class: "d-ic danger" }, icon("alert", 26)),
        h("h2", {}, tr("⚠ Possible attack detected")),
        h("p", {}, trf("The certificate of {address} changed since the last connection. Someone may be impersonating the server. Only continue if the administrator confirms the change.", { address: event.address })),
        h("div", { class: "dialog-extra" },
          h("div", { class: "cert-label" }, tr("Previous fingerprint")), groups(event.previous ?? ""),
          h("div", { class: "cert-label" }, tr("Current fingerprint")), groups(event.fingerprint))),
      h("div", { class: "dialog-actions" }, h("button", { class: "btn danger-soft", onclick: () => { accepted = true; close(); } }, tr("Trust anyway")), cancel)), { onClose: answer });
    requestAnimationFrame(() => cancel.focus());
  } else {
    const accept = button(tr("Trust and connect"), { kind: "primary", onClick: () => { accepted = true; close(); } });
    const close = overlay(h("div", { class: "dialog", role: "alertdialog", "aria-label": tr("Verify server identity") },
      h("div", { class: "dialog-body" },
        h("div", { class: "d-ic" }, icon("certificate", 26)),
        h("h2", {}, tr("Verify server identity")),
        h("p", {}, trf("This is the first connection to {address}. Confirm with the server administrator that this fingerprint is the same:", { address: event.address })),
        h("div", { class: "dialog-extra" }, h("div", { class: "cert-label" }, tr("Certificate fingerprint (SHA-256)")), groups(event.fingerprint))),
      h("div", { class: "dialog-actions" }, h("button", { class: "btn", onclick: () => close() }, tr("Cancel")), accept)), { onClose: answer });
    requestAnimationFrame(() => accept.focus());
  }
}

function passwordDialog(event) {
  let result = null;
  const pass = h("input", { class: "input", type: "password", "aria-label": tr("Password"), autocomplete: "current-password" });
  const reveal = iconButton("eye", tr("Show password"), () => {
    const show = pass.type === "password";
    pass.type = show ? "text" : "password";
    clear(reveal, icon(show ? "eye-off" : "eye", 17));
    pass.focus();
  }, "sm");
  let keep = true;
  const go = button(tr("Connect"), { kind: "primary", disabled: true, onClick: () => { result = pass.value; close(); } });
  pass.addEventListener("input", () => (go.disabled = !pass.value));
  pass.addEventListener("keydown", (e) => { if (e.key === "Enter" && pass.value) go.click(); });
  const close = overlay(h("div", { class: "dialog", role: "dialog", "aria-label": tr("Authentication required") },
    h("div", { class: "dialog-body" },
      h("div", { class: "d-ic" }, icon("lock", 26)),
      h("h2", {}, tr("Authentication required")),
      h("p", {}, trf("Enter the password for account “{username}” to continue.", { username: event.username })),
      h("div", { class: "dialog-extra" },
        h("div", { class: "input-reveal" }, pass, reveal),
        h("label", { class: "row-flex small keyring-row" },
          h("span", {}, h("strong", {}, tr("Save to keyring")), h("br"), h("span", { class: "muted" }, tr("Use the system Secret Service so you are not asked again"))),
          toggle(keep, (v) => (keep = v), tr("Save to keyring"))))),
    h("div", { class: "dialog-actions" }, h("button", { class: "btn", onclick: () => close() }, tr("Cancel")), go)),
  { onClose: () => call("session_answer_creds", { id: event.id, password: result, save: keep }) });
  requestAnimationFrame(() => pass.focus());
}

// ---------- Eventos da sessão ----------

function onSessionEvent(event) {
  switch (event.kind) {
    case "connected":
      setState("connected");
      showOverlay(null);
      captured = true;
      renderBar();
      canvas.focus();
      toast(tr("Connected. Ctrl+Alt+F12 releases the keyboard and mouse."), { ic: "check-circle" });
      devSnapshot();
      break;
    case "cert":
      certificateDialog(event);
      break;
    case "creds":
      passwordDialog(event);
      break;
    case "notice":
      $("notice").hidden = false;
      clear($("notice"), icon("info", 16), h("span", {}, event.text), iconButton("x", tr("Close"), () => ($("notice").hidden = true), "sm"));
      break;
    case "lost":
    case "failed":
      setState("lost");
      document.querySelectorAll(".overlay").forEach((o) => o.remove());
      showOverlay(lostCard(event.kind, event.text));
      break;
  }
}

// ---------- Entrada ----------

let pendingMove = null;
function remotePoint(e) {
  const rect = canvas.getBoundingClientRect();
  if (!rect.width || !rect.height) return null;
  const x = Math.round(((e.clientX - rect.left) / rect.width) * canvas.width);
  const y = Math.round(((e.clientY - rect.top) / rect.height) * canvas.height);
  return { x: Math.max(0, Math.min(canvas.width - 1, x)), y: Math.max(0, Math.min(canvas.height - 1, y)) };
}
const send = (events) => call("session_input", { events }).catch(() => {});
const active = () => state === "connected" && captured;

canvas.addEventListener("mousemove", (e) => {
  if (!active()) return;
  const p = remotePoint(e);
  if (!p) return;
  // Uma posição por quadro: o beam-core também só guarda a mais recente.
  if (!pendingMove) requestAnimationFrame(() => { send([{ t: "move", ...pendingMove }]); pendingMove = null; });
  pendingMove = p;
});
canvas.addEventListener("mousedown", (e) => {
  if (state !== "connected") return;
  if (!captured) { captured = true; renderBar(); canvas.focus(); }
  const p = remotePoint(e);
  if (p && e.button <= 2) { e.preventDefault(); send([{ t: "button", ...p, button: e.button, pressed: true }]); }
});
canvas.addEventListener("mouseup", (e) => {
  if (!active()) return;
  const p = remotePoint(e);
  if (p && e.button <= 2) { e.preventDefault(); send([{ t: "button", ...p, button: e.button, pressed: false }]); }
});
canvas.addEventListener("contextmenu", (e) => e.preventDefault());
canvas.addEventListener("wheel", (e) => {
  if (!active()) return;
  e.preventDefault();
  const p = remotePoint(e);
  // 120 unidades RDP por "dente"; rolar para baixo é negativo, como no GTK.
  const notches = e.deltaMode === 0 ? e.deltaY / 100 : e.deltaY / 3;
  const steps = Math.max(-120, Math.min(120, Math.round(-notches * 120)));
  if (p && steps) send([{ t: "wheel", ...p, steps }]);
}, { passive: false });

function release() {
  if (pressed.size) send([...pressed.values()].map(([scancode, extended]) => ({ t: "key", scancode, extended, pressed: false })));
  pressed.clear();
}

document.addEventListener("keydown", (e) => {
  if (document.querySelector(".overlay")) return;
  if (e.ctrlKey && e.altKey && e.code === "F12") {
    e.preventDefault();
    release();
    captured = false;
    if (fullscreen) toggleFullscreen();
    renderBar();
    toast(tr("Keyboard and mouse released. Click the session to capture them again."), { ic: "keyboard" });
    return;
  }
  if (!active()) return;
  const sc = scancodeFor(e.code);
  e.preventDefault();
  if (!sc) return;
  pressed.set(e.code, sc);
  send([{ t: "key", scancode: sc[0], extended: sc[1], pressed: true }]);
}, true);
document.addEventListener("keyup", (e) => {
  const sc = pressed.get(e.code) ?? (active() ? scancodeFor(e.code) : null);
  if (!sc) return;
  e.preventDefault();
  pressed.delete(e.code);
  send([{ t: "key", scancode: sc[0], extended: sc[1], pressed: false }]);
}, true);
// Sem foco, nenhuma tecla pode ficar "presa" no Windows.
window.addEventListener("blur", release);

// ---------- Janela ----------

async function toggleFullscreen() {
  fullscreen = !(await win.isFullscreen());
  await win.setFullscreen(fullscreen);
  document.body.classList.toggle("fullscreen", fullscreen);
  renderBar();
  canvas.focus();
}

function setupWindow() {
  const make = (name, label, fn) => h("button", { class: "wc", "aria-label": label, title: label, onclick: fn }, icon(name, 13));
  $("window-controls").append(make("minus", tr("Minimize"), () => win.minimize()), make("maximize", tr("Maximize"), () => win.toggleMaximize()), make("x", tr("Close"), () => win.close()));
  document.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || !e.target.closest("[data-drag]") || e.target.closest("button")) return;
    if (e.detail === 2) win.toggleMaximize(); else win.startDragging();
  });
  for (const dir of ["North", "South", "East", "West", "NorthEast", "NorthWest", "SouthEast", "SouthWest"]) {
    document.body.append(h("div", { class: "resize", dataset: { dir }, onmousedown: (e) => { e.preventDefault(); win.startResizeDragging(dir); } }));
  }
  // Em tela cheia, a barra aparece ao levar o ponteiro ao topo.
  document.addEventListener("mousemove", (e) => { if (fullscreen) $("bar").classList.toggle("peek", e.clientY < 8 || $("bar").matches(":hover")); });
}

/** Só em builds de debug: BEAM_DEV_TYPE digita pelo caminho do teclado e
 * BEAM_DEV_SNAPSHOT grava o canvas para testes. */
async function devSnapshot() {
  const text = await call("dev_type_text").catch(() => null);
  if (text) {
    await new Promise((r) => setTimeout(r, 2000));
    for (const ch of text.replace(/\\n/g, "\n")) {
      const code = ch === "\n" ? "Enter" : ch === "-" ? "Minus" : /[0-9]/.test(ch) ? `Digit${ch}` : `Key${ch.toUpperCase()}`;
      const sc = scancodeFor(code);
      const shift = /[A-Z]/.test(ch);
      if (shift) await send([{ t: "key", scancode: 0x2a, extended: false, pressed: true }]);
      await send([{ t: "key", scancode: sc[0], extended: sc[1], pressed: true }, { t: "key", scancode: sc[0], extended: sc[1], pressed: false }]);
      if (shift) await send([{ t: "key", scancode: 0x2a, extended: false, pressed: false }]);
    }
  }
  if (!(await call("dev_snapshot_path").catch(() => null))) return;
  setTimeout(() => {
    call("dev_dump_framebuffer").catch((e) => call("app_log", { message: `dump: ${e.message}` }));
    call("dev_snapshot", { dataUrl: canvas.toDataURL("image/png") }).catch(() => {});
  }, text ? 9000 : 4000);
}

async function start() {
  const app = await call("app_info");
  await loadCatalog(call, app.locale);
  setupWindow();
  on("session-event", onSessionEvent);
  const channel = newChannel();
  channel.onmessage = paint;
  info = await call("session_open", { frames: channel });
  document.title = `${info.name} — Beam`;
  $("name").textContent = info.name;
  canvas.width = info.width;
  canvas.height = info.height;
  canvas.setAttribute("aria-label", tr("Remote desktop"));
  ctx.fillStyle = "#101014";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  fit();
  showOverlay(connectingCard());
  renderBar();
  if (info.fullscreen) toggleFullscreen();
}

start().catch((error) => call("app_log", { message: `session: ${error?.stack ?? error}` }).catch(() => {}));
