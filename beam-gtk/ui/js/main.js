// Beam — gerenciador de conexões (Tauri, no padrão do Vega). Conexões salvas
// em ~/.config (beam-core), certificados confiáveis (TOFU) e abertura das
// janelas de sessão; as regras seguem a antiga interface GTK.

import { call, on, win } from "./api.js";
import { svg, registerIcons } from "./icons.js";
import { loadCatalog, tr, trf } from "./i18n.js";
import {
  h, icon, clear, normalize, button, iconButton, badge, toggle, segmented, list, row, banner, empty,
  toast, overlay, confirm, sheet, popover, menuItem, fail, busy,
} from "./ui.js";

registerIcons({
  beam: '<path d="M4 18 18 4"/><path d="M14 4h4v4"/><circle cx="6" cy="18" r="2.5"/><path d="M9.5 18H20"/>',
  "screen-share": '<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8M12 17v4"/><path d="m9 10 3-3 3 3M12 7v6"/>',
  certificate: '<path d="M14 15H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v8"/><path d="M7 7h10M7 11h4"/><circle cx="18" cy="17" r="3"/><path d="m16.5 19.5-1 3 2.5-1.2 2.5 1.2-1-3"/>',
  "list-all": '<path d="M3 6h.01M3 12h.01M3 18h.01M8 6h13M8 12h13M8 18h13"/>',
  grid: '<rect width="7" height="7" x="3" y="3" rx="1.5"/><rect width="7" height="7" x="14" y="3" rx="1.5"/><rect width="7" height="7" x="14" y="14" rx="1.5"/><rect width="7" height="7" x="3" y="14" rx="1.5"/>',
});

const $ = (id) => document.getElementById(id);
const PRESETS = [[1280, 720], [1366, 768], [1600, 900], [1920, 1080], [2560, 1440]];

let view = "connections";
let layout = "grid";
let filter = "";
let profiles = [];
let loadError = null;
let trusted = [];
let trustedError = null;
let live = new Map(); // profile id → "connecting" | "connected" | "lost"

// ---------- Tema (segue o GNOME) ----------

const ACCENTS = {
  blue: "#3584e4", teal: "#2190a4", green: "#3a944a", yellow: "#c88800", orange: "#ed5b00",
  red: "#e62d42", pink: "#d56199", purple: "#9141ac", slate: "#6f8396",
};
function applyInterface({ color_scheme, accent } = {}) {
  const dark = color_scheme === "prefer-dark";
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  let color = ACCENTS[accent] ?? ACCENTS.blue;
  if (dark) color = `color-mix(in srgb, ${color} 78%, #fff)`;
  document.documentElement.style.setProperty("--accent", color);
}

/** Matiz estável por nome, para a miniatura de cada conexão. */
function hueFor(text) {
  let hash = 0;
  for (const ch of text) hash = (hash * 31 + ch.charCodeAt(0)) | 0;
  return String(Math.abs(hash) % 360);
}
const initials = (name) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0].toUpperCase()).join("") || "?";
const address = (p) => `${p.username ? `${p.domain ? `${p.domain}\\` : ""}${p.username}@` : ""}${p.host}${p.port !== 3389 ? `:${p.port}` : ""}`;
const resLabel = (p) => `${p.resolution.width}×${p.resolution.height}`;

// ---------- Conexões ----------

async function connect(p) {
  try { await call("connect", { id: p.id }); } catch (error) { fail(error); }
}

async function duplicate(p) {
  try {
    const copy = await call("profile_duplicate", { id: p.id });
    await load();
    openProfile(profiles.find((x) => x.id === copy.id) ?? copy);
  } catch (error) { fail(error); }
}

async function remove(p) {
  const ok = await confirm({
    title: tr("Delete the connection?"), text: p.name, ic: "trash", tone: "danger", confirmLabel: tr("Delete"), confirmKind: "danger",
    facts: [
      { icon: "key", text: tr("The password saved in the keyring is deleted too, unless another connection uses the same user and server.") },
      { icon: "certificate", text: tr("The server's trusted certificate is kept.") },
    ],
  });
  if (!ok) return;
  try { await call("profile_delete", { id: p.id }); toast(tr("Connection deleted.")); load(); } catch (error) { fail(error); }
}

function connectionMenu(anchor, p) {
  const pop = popover(anchor, [
    menuItem("screen-share", tr("Connect"), () => { pop.close(); connect(p); }, "Enter"),
    h("hr"),
    menuItem("cog", tr("Edit"), () => { pop.close(); openProfile(p); }, "F2"),
    menuItem("copy", tr("Duplicate"), () => { pop.close(); duplicate(p); }),
    h("hr"),
    menuItem("trash", tr("Delete"), () => { pop.close(); remove(p); }, "Del"),
  ], "menu");
}

function cardKeys(e, p) {
  if (e.target !== e.currentTarget) return;
  if (e.key === "Enter") { e.preventDefault(); connect(p); }
  else if (e.key === "Delete") { e.preventDefault(); remove(p); }
  else if (e.key === "F2") { e.preventDefault(); openProfile(p); }
  else if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) { e.preventDefault(); connectionMenu(e.currentTarget.querySelector(".more"), p); }
}

function liveBadge(p) {
  const state = live.get(p.id);
  if (state === "connected") return h("span", { class: "conn-live" }, h("i"), tr("Connected"));
  if (state === "connecting") return h("span", { class: "conn-live connecting" }, h("span", { class: "spinner" }), tr("Connecting…"));
  if (state === "lost") return h("span", { class: "conn-live lost" }, h("i"), tr("Connection lost"));
  return null;
}

function connectionCard(p) {
  const state = live.get(p.id);
  return h("div", {
    class: `conn-card ${state === "connected" ? "live" : ""}`.trim(), tabindex: "0", role: "listitem", "aria-label": `${p.name}, ${address(p)}`,
    ondblclick: () => connect(p), onkeydown: (e) => cardKeys(e, p),
    oncontextmenu: (e) => { e.preventDefault(); connectionMenu(e.currentTarget.querySelector(".more"), p); },
  },
    h("div", { class: "conn-top" },
      h("span", { class: "conn-screen", style: { "--hue": hueFor(p.name) } }, h("span", { class: "initials" }, initials(p.name))),
      h("div", { class: "conn-name" }, h("strong", { title: p.name }, p.name), h("span", { title: address(p) }, address(p))),
      h("button", { class: "icon-btn sm more", "aria-label": tr("Connection actions"), title: tr("Connection actions"), onclick: (e) => { e.stopPropagation(); connectionMenu(e.currentTarget, p); } }, icon("more", 17))),
    h("div", { class: "conn-meta" },
      badge(resLabel(p), "outline", "monitor"),
      badge(trf("{bits} bits", { bits: p.color_depth }), "outline"),
      p.fullscreen && badge(tr("Fullscreen"), "outline", "maximize")),
    h("div", { class: "conn-foot" },
      state
        ? [liveBadge(p), h("span", { class: "spacer" }), button(tr("Show session"), { size: "sm", icon: "external", onClick: () => connect(p) })]
        : button(tr("Connect"), { kind: "primary", icon: "screen-share", onClick: () => connect(p) })),
  );
}

function connectionRow(p) {
  return h("div", { class: "list-row clickable", tabindex: "0", ondblclick: () => connect(p), onkeydown: (e) => cardKeys(e, p),
    oncontextmenu: (e) => { e.preventDefault(); connectionMenu(e.currentTarget.querySelector(".more"), p); } },
    h("span", { class: "lead screen", style: { "--hue": hueFor(p.name) } }, initials(p.name)),
    h("div", { class: "body" }, h("div", { class: "title" }, p.name), h("div", { class: "subtitle" }, `${address(p)} · ${resLabel(p)}`)),
    h("div", { class: "suffix" },
      liveBadge(p) ?? button(tr("Connect"), { size: "sm", kind: "primary", onClick: () => connect(p) }),
      h("button", { class: "icon-btn sm more", "aria-label": tr("Connection actions"), onclick: (e) => { e.stopPropagation(); connectionMenu(e.currentTarget, p); } }, icon("more", 17))));
}

function renderConnections() {
  const errorBanner = loadError && h("div", { style: { "margin-bottom": "16px" } }, banner("danger", "alert-circle", tr("Could not load connections"), loadError));
  const matches = (p) => !filter || [p.name, p.host, p.username].some((v) => normalize(v ?? "").includes(normalize(filter)));
  const shown = profiles.filter(matches);
  if (!profiles.length) {
    return [errorBanner, empty("screen-share", tr("No connections"), tr("Create your first connection to get started"),
      button(tr("New connection"), { kind: "primary pill", icon: "plus", onClick: () => openProfile() }))];
  }
  if (!shown.length) return empty("search", tr("Nothing found"), trf("No connection matches “{query}”.", { query: filter }));
  return [errorBanner, layout === "grid"
    ? h("div", { class: "conn-grid", role: "list" }, shown.map(connectionCard))
    : h("div", { class: "list conn-list" }, shown.map(connectionRow))];
}

// ---------- Nova conexão / editar ----------

function openProfile(existing) {
  const p = existing ? structuredClone(existing)
    : { id: "00000000-0000-0000-0000-000000000000", name: "", host: "", port: 3389, username: "", domain: null, resolution: { width: 1366, height: 768 }, color_depth: 32, fullscreen: true };
  const input = (value, attrs = {}) => { const el = h("input", { class: "input", ...attrs }); el.value = value ?? ""; return el; };
  const name = input(p.name, { "aria-label": tr("Connection name") });
  const host = input(p.host, { placeholder: tr("name.domain.local or 192.168.0.10"), "aria-label": tr("Server (host)"), class: "input mono", spellcheck: "false" });
  const port = input(String(p.port), { type: "number", min: "1", max: "65535", "aria-label": tr("Port"), class: "input port" });
  const user = input(p.username, { "aria-label": tr("Username"), spellcheck: "false", autocomplete: "off" });
  const domain = input(p.domain, { "aria-label": tr("Domain (optional)"), spellcheck: "false" });
  const err = h("p", { class: "hint err", role: "alert" });

  let res = PRESETS.some(([w, hh]) => w === p.resolution.width && hh === p.resolution.height) ? `${p.resolution.width}x${p.resolution.height}` : "custom";
  const cw = input(String(p.resolution.width), { type: "number", min: "320", max: "7680", "aria-label": tr("Custom width") });
  const ch = input(String(p.resolution.height), { type: "number", min: "240", max: "4320", "aria-label": tr("Custom height") });
  const custom = h("div", { class: "row-flex", hidden: res !== "custom" }, cw, h("span", { class: "muted" }, "×"), ch);
  const resGrid = h("div", { class: "res-grid", role: "radiogroup", "aria-label": tr("Resolution") });
  const paintRes = () => {
    clear(resGrid, [...PRESETS.map(([w, hh]) => [`${w}x${hh}`, `${w}×${hh}`, w, hh]), ["custom", tr("Custom…"), 0, 0]].map(([id, label, w, hh]) =>
      h("button", { class: "res-opt", role: "radio", "aria-checked": String(res === id), onclick: () => { res = id; custom.hidden = id !== "custom"; paintRes(); if (id === "custom") cw.focus(); } },
        id === "custom" ? icon("cog", 22) : h("span", { class: "shape", style: { width: `${Math.round(w / 64)}px`, height: `${Math.round(hh / 64)}px` } }), label)));
  };
  paintRes();
  let depth = p.color_depth;
  let full = p.fullscreen;

  const save = button(tr("Save"), { kind: "primary", disabled: !p.host || !p.name });
  const validate = () => (save.disabled = !host.value.trim() || !name.value.trim());
  name.addEventListener("input", validate);
  host.addEventListener("input", validate);
  const close = sheet(existing ? tr("Edit connection") : tr("New connection"), [
    h("div", { class: "form-section" }, h("h4", {}, icon("globe", 15), tr("Connection")),
      h("div", { class: "form-grid" },
        h("div", { class: "field full" }, h("label", {}, tr("Connection name")), name),
        h("div", { class: "field" }, h("label", {}, tr("Server (host)")), host),
        h("div", { class: "field" }, h("label", {}, tr("Port")), port))),
    h("div", { class: "form-section" }, h("h4", {}, icon("user", 15), tr("Authentication")),
      h("div", { class: "form-grid" },
        h("div", { class: "field" }, h("label", {}, tr("Username")), user),
        h("div", { class: "field" }, h("label", {}, tr("Domain (optional)")), domain),
        h("p", { class: "hint full" }, tr("The password is asked when connecting and can be kept in the system keyring. It is never written to a file.")))),
    h("div", { class: "form-section" }, h("h4", {}, icon("monitor", 15), tr("Display")),
      h("div", { class: "field" }, h("span", { class: "label" }, tr("Resolution")), resGrid, custom),
      h("div", { class: "form-grid", style: { "margin-top": "14px" } },
        h("div", { class: "field" }, h("span", { class: "label" }, tr("Color depth")), segmented([{ id: 16, label: tr("16 bits") }, { id: 32, label: tr("32 bits") }], depth, (v) => (depth = v))),
        h("div", { class: "field" }, h("span", { class: "label" }, tr("Open in fullscreen")), h("label", { class: "row-flex small" }, toggle(full, (v) => (full = v), tr("Open in fullscreen")))))),
    err,
    h("div", { class: "sheet-actions" }, button(tr("Cancel"), { onClick: () => close() }), save),
  ]);
  save.addEventListener("click", () => busy(save, async () => {
    const resolution = res === "custom"
      ? { width: Math.round(Number(cw.value)), height: Math.round(Number(ch.value)) }
      : { width: Number(res.split("x")[0]), height: Number(res.split("x")[1]) };
    const values = { ...p, name: name.value, host: host.value, port: Math.round(Number(port.value)), username: user.value.trim(), domain: domain.value || null, resolution, color_depth: depth, fullscreen: full };
    if (!(values.port >= 1 && values.port <= 65535)) { err.textContent = tr("The port goes from 1 to 65535."); return; }
    try {
      await call("profile_save", { profile: values });
      close();
      toast(existing ? tr("Connection saved.") : tr("Connection created."));
      load();
    } catch (error) {
      err.textContent = error.message;
    }
  }));
  requestAnimationFrame(() => (existing ? host : name).focus());
}

// ---------- Certificados confiáveis ----------

function renderCertificates() {
  if (trustedError) return banner("danger", "alert-circle", tr("Could not load trusted certificates"), trustedError);
  if (!trusted.length) return empty("certificate", tr("No trusted certificates yet"), tr("Certificate fingerprints confirmed on previous connections (TOFU verification). Removing an entry requires confirmation again the next time you connect to that server."));
  return [
    h("p", { class: "muted", style: { margin: "6px 2px 16px", "max-width": "70ch" } }, tr("Certificate fingerprints confirmed on previous connections (TOFU verification). Removing an entry requires confirmation again the next time you connect to that server.")),
    list(trusted.map((c) => row({
      icon: "certificate", tint: "green", title: c.address, subtitle: c.fingerprint, cls: "cert-row",
      suffix: iconButton("trash", tr("Remove trust"), async () => {
        const ok = await confirm({ title: tr("Remove trust?"), text: c.address, ic: "certificate", confirmLabel: tr("Remove trust"), confirmKind: "danger",
          facts: [{ icon: "info", text: tr("Next time, Beam shows the fingerprint again for you to confirm.") }] });
        if (!ok) return;
        try { await call("trusted_remove", { address: c.address }); toast(tr("Trust removed.")); load(); } catch (error) { fail(error); }
      }),
    }))),
  ];
}

// ---------- Moldura ----------

const VIEWS = [
  { id: "connections", title: () => tr("Connections"), icon: "screen-share" },
  { id: "certificates", title: () => tr("Trusted certificates"), icon: "certificate" },
];

function renderNav() {
  const counts = { connections: profiles.length, certificates: trusted.length };
  clear($("nav"), h("div", { class: "nav-group" }, VIEWS.map((v) =>
    h("button", { class: "nav-item", "aria-current": v.id === view ? "page" : null, onclick: () => navigate(v.id) },
      icon(v.icon, 18), h("span", {}, v.title()), counts[v.id] ? h("span", { class: "count muted-count" }, String(counts[v.id])) : null))));
  const open = [...live.values()].filter((s) => s === "connected").length;
  clear($("keyring-chip"), h("span", { class: "lead" }, icon("key", 17)),
    h("div", {}, h("strong", {}, tr("System keyring")), h("span", {}, open ? trf("{n} sessions open", { n: open }) : tr("Passwords never go to a file"))));
}

function renderHeader() {
  if (view !== "connections") { clear($("header-actions"), h("span", { class: "header-sep" })); return; }
  const search = h("input", { class: "input", placeholder: tr("Search connections"), "aria-label": tr("Search connections") });
  search.id = "filter";
  search.value = filter;
  search.addEventListener("input", () => { filter = search.value; renderPage(); });
  search.addEventListener("keydown", (e) => { if (e.key === "Escape") { search.value = ""; filter = ""; renderPage(); search.blur(); } });
  const lay = segmented([{ id: "grid", icon: "grid", label: "" }, { id: "list", icon: "list-all", label: "" }], layout, (v) => {
    layout = v;
    try { localStorage.setItem("beam.layout", v); } catch {}
    renderPage();
  });
  lay.querySelectorAll("button").forEach((b, i) => { b.setAttribute("aria-label", i ? tr("List") : tr("Grid")); b.title = i ? tr("List") : tr("Grid"); });
  clear($("header-actions"),
    h("div", { class: "input-icon header-filter" }, icon("search", 15), search),
    lay,
    button(tr("New connection"), { kind: "primary", icon: "plus", onClick: () => openProfile(), title: "Ctrl N" }),
    h("span", { class: "header-sep" }));
}

function renderPage() {
  const focused = document.activeElement?.closest?.("[data-pid]")?.dataset.pid;
  clear($("page"), view === "connections" ? renderConnections() : renderCertificates());
  $("page-crumb").textContent = view === "connections" ? trf("{n} connections", { n: profiles.length }) : trf("{n} servers", { n: trusted.length });
  if (focused) document.querySelector(`[data-pid="${focused}"]`)?.focus();
}

function navigate(id) {
  view = id;
  $("page-title").textContent = VIEWS.find((v) => v.id === id).title();
  renderNav(); renderHeader(); renderPage();
  $("page").classList.remove("enter"); void $("page").offsetWidth; $("page").classList.add("enter");
}

async function load() {
  const [list, certs] = await Promise.all([call("profiles_list"), call("trusted_list").then((c) => ({ c })).catch((e) => ({ e }))]);
  profiles = list.profiles;
  loadError = list.load_error;
  trusted = certs.c ?? [];
  trustedError = certs.e?.message ?? null;
  renderNav(); renderPage();
}

function openMainMenu(anchor) {
  const pop = popover(anchor, [
    menuItem("plus", tr("New connection"), () => { pop.close(); openProfile(); }, "Ctrl N"),
    menuItem("certificate", tr("Trusted certificates"), () => { pop.close(); navigate("certificates"); }),
    h("hr"),
    menuItem("keyboard", tr("Keyboard shortcuts"), () => { pop.close(); openShortcuts(); }, "Ctrl ?"),
    menuItem("star", tr("About Beam"), () => { pop.close(); openAbout(); }),
  ], "menu");
}

function openShortcuts() {
  const rows = [[tr("New connection"), "Ctrl N"], [tr("Search"), "Ctrl K"], [tr("Connect to the selected connection"), "Enter"],
    [tr("Edit the selected connection"), "F2"], [tr("Delete the selected connection"), "Del"],
    [tr("In a session: release keyboard and mouse, leave fullscreen"), "Ctrl Alt F12"], [tr("Close dialog"), "Esc"]];
  sheet(tr("Keyboard shortcuts"), list(rows.map(([label, keys]) => h("div", { class: "list-row" }, h("div", { class: "body" }, label), h("div", { class: "suffix" }, keys.split(" ").map((k) => h("kbd", {}, k)))))), { wide: false });
}

let appInfo = { version: "" };
function openAbout() {
  sheet(tr("About Beam"), [
    h("div", { style: { "text-align": "center", padding: "6px 0 4px" } },
      h("div", { class: "brand-mark", style: { width: "72px", height: "72px", "border-radius": "20px", margin: "0 auto 12px" } }, icon("beam", 38)),
      h("h2", { style: { "font-size": "22px", "font-weight": "800" } }, "Beam"),
      h("p", { class: "muted" }, appInfo.version),
      h("p", { class: "muted" }, tr("Remote desktop client (RDP) for the Lyra OS ecosystem.")),
      h("p", { class: "small faint" }, "IronRDP · Lyra OS · GPL-3.0-or-later")),
  ], { wide: false });
}

function openPalette() {
  const input = h("input", { placeholder: tr("Search connections or actions…"), "aria-label": tr("Search") });
  const listEl = h("div", { class: "palette-list", role: "listbox" });
  let results = [];
  let selected = 0;
  const render = () => {
    const q = normalize(input.value.trim());
    const conns = profiles.filter((p) => !q || normalize(`${p.name} ${p.host} ${p.username}`).includes(q))
      .map((p) => ({ section: tr("Connect"), lead: h("span", { class: "conn-screen mini", style: { "--hue": hueFor(p.name) } }), label: p.name, where: p.host, run: () => connect(p) }));
    const acts = [{ icon: "plus", label: tr("New connection"), run: () => openProfile() }, { icon: "certificate", label: tr("Trusted certificates"), run: () => navigate("certificates") }]
      .filter((a) => !q || normalize(a.label).includes(q)).map((a) => ({ ...a, section: tr("Actions") }));
    results = [...conns, ...acts];
    selected = Math.min(selected, Math.max(0, results.length - 1));
    let last = null;
    clear(listEl, results.map((r, i) => {
      const head = r.section !== last && h("div", { class: "palette-section" }, r.section);
      last = r.section;
      return [head, h("button", { class: "palette-item", role: "option", "aria-selected": String(i === selected), onclick: () => { close(); r.run(); } },
        r.lead ?? icon(r.icon, 17), h("span", {}, r.label), r.where && h("span", { class: "where" }, r.where))];
    }));
  };
  input.addEventListener("input", () => { selected = 0; render(); });
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); selected = Math.min(results.length - 1, selected + 1); render(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); selected = Math.max(0, selected - 1); render(); }
    else if (e.key === "Enter") { e.preventDefault(); const r = results[selected]; if (r) { close(); r.run(); } }
  });
  const close = overlay(h("div", { class: "palette", role: "dialog", "aria-label": tr("Search") },
    h("div", { class: "palette-input" }, icon("search", 19), input, h("kbd", {}, "Esc")), listEl), { top: true });
  render();
  requestAnimationFrame(() => input.focus());
}

function setupWindow() {
  const make = (name, label, fn) => h("button", { class: "wc", "aria-label": label, title: label, onclick: fn }, icon(name, 13));
  const maxBtn = make("maximize", tr("Maximize"), () => win.toggleMaximize());
  $("window-controls").append(make("minus", tr("Minimize"), () => win.minimize()), maxBtn, make("x", tr("Close"), () => win.close()));
  document.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || !e.target.closest("[data-drag]") || e.target.closest("button, input, select")) return;
    if (e.detail === 2) win.toggleMaximize(); else win.startDragging();
  });
  for (const dir of ["North", "South", "East", "West", "NorthEast", "NorthWest", "SouthEast", "SouthWest"]) {
    document.body.append(h("div", { class: "resize", dataset: { dir }, onmousedown: (e) => { e.preventDefault(); win.startResizeDragging(dir); } }));
  }
  const sync = async () => {
    const max = await win.isMaximized();
    document.body.classList.toggle("maximized", max);
    clear(maxBtn, icon(max ? "restore" : "maximize", 13));
  };
  win.onResized(sync);
  sync();
  document.addEventListener("keydown", (e) => {
    if (document.querySelector(".overlay")) return;
    const mod = e.ctrlKey || e.metaKey;
    const k = e.key.toLowerCase();
    if (mod && k === "n") { e.preventDefault(); openProfile(); }
    else if (mod && k === "k") { e.preventDefault(); openPalette(); }
    else if (mod && k === "f") { e.preventDefault(); $("filter")?.focus(); }
    else if (mod && e.key === "?") { e.preventDefault(); openShortcuts(); }
    else if (mod && (k === "w" || k === "q")) { e.preventDefault(); win.close(); }
  });
}

function applySessions(list) {
  live = new Map(list.map((s) => [s.profile_id, s.state]));
  renderNav();
  if (view === "connections") renderPage();
}

async function start() {
  appInfo = await call("app_info");
  await loadCatalog(call, appInfo.locale);
  applyInterface(await call("interface_style").catch(() => ({})));
  on("interface-changed", applyInterface);
  try { layout = localStorage.getItem("beam.layout") === "list" ? "list" : "grid"; } catch {}
  $("brand-mark").innerHTML = svg("beam", 15);
  $("search-ic").innerHTML = svg("search", 15);
  $("menu-btn").innerHTML = svg("menu", 17);
  $("sidebar-toggle").innerHTML = svg("sidebar", 17);
  $("search-label").textContent = tr("Search connections");
  $("menu-btn").title = $("menu-btn").ariaLabel = tr("Main menu");
  $("sidebar-toggle").title = $("sidebar-toggle").ariaLabel = tr("Show navigation");
  $("menu-btn").addEventListener("click", (e) => openMainMenu(e.currentTarget));
  $("search-trigger").addEventListener("click", () => openPalette());
  $("sidebar-toggle").addEventListener("click", () => $("sidebar").classList.toggle("open"));
  $("scroller").addEventListener("scroll", (e) => $("headerbar").classList.toggle("scrolled", e.target.scrollTop > 4), { passive: true });
  setupWindow();
  await load();
  navigate("connections");
  on("sessions-changed", applySessions);
  applySessions(await call("sessions_state"));
  // Só em builds de debug (BEAM_DEV_CONNECT): conecta sozinho para testes.
  const auto = await call("dev_autoconnect").catch(() => null);
  const target = auto && profiles.find((p) => p.id === auto);
  if (target) connect(target);
}

start().catch((error) => call("app_log", { message: `start: ${error?.stack ?? error}` }).catch(() => {}));
