// Componentes de interface. Tudo é montado com nós DOM (nunca innerHTML com
// dados), então mensagens do journal ou nomes de pacote não viram HTML.

import { svg } from "./icons.js";
import { tr, trf, locale } from "./i18n.js";
import { prefs } from "./store.js";

export function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props ?? {})) {
    if (value == null || value === false) continue;
    if (key === "class") el.className = value;
    else if (key === "text") el.textContent = value;
    else if (key.startsWith("on")) el.addEventListener(key.slice(2).toLowerCase(), value);
    else if (key === "style") for (const [p, v] of Object.entries(value)) el.style.setProperty(p, v);
    else if (key === "dataset") Object.assign(el.dataset, value);
    else if (value === true) el.setAttribute(key, "");
    else el.setAttribute(key, value);
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const child of children.flat(Infinity)) {
    if (child == null || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
}

export function icon(name, size = 16, cls = "ic") {
  const span = document.createElement("span");
  span.className = cls;
  span.innerHTML = svg(name, size);
  return span;
}

export function clear(el, ...children) {
  el.replaceChildren();
  append(el, children);
  return el;
}

// ---------- Formatação ----------

export const num = (n, digits = 1) => new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(n);

export function bytes(n, digits = 1) {
  if (!n) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${num(n / 1024 ** i, i < 2 ? 0 : digits)} ${units[i]}`;
}

export function rate(bps) {
  return `${bytes(bps)}/s`;
}

export function duration(secs) {
  const d = Math.floor(secs / 86400);
  const hrs = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d) return trf("{days} d {hours} h", { days: d, hours: hrs });
  if (hrs) return trf("{hours} h {minutes} min", { hours: hrs, minutes: m });
  return trf("{minutes} min", { minutes: m });
}

export function relative(date) {
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  const diff = (new Date(date).getTime() - Date.now()) / 1000;
  const abs = Math.abs(diff);
  if (abs < 60) return tr("now");
  if (abs < 3600) return rtf.format(Math.round(diff / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(diff / 3600), "hour");
  if (abs < 86400 * 30) return rtf.format(Math.round(diff / 86400), "day");
  return rtf.format(Math.round(diff / (86400 * 30)), "month");
}

export const dayLabel = (date) => {
  const d = new Date(date);
  const today = new Date();
  const yesterday = new Date(Date.now() - 86400000);
  if (d.toDateString() === today.toDateString()) return tr("Today");
  if (d.toDateString() === yesterday.toDateString()) return tr("Yesterday");
  return d.toLocaleDateString(locale, { weekday: "long", day: "numeric", month: "long" });
};
export const time = (date) => new Date(date).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
export const dateTime = (date) => new Date(date).toLocaleString(locale, { day: "2-digit", month: "2-digit", year: "numeric", hour: "2-digit", minute: "2-digit" });

export function normalize(text) {
  return String(text).normalize("NFD").replace(/\p{Diacritic}/gu, "").toLowerCase();
}

// ---------- Componentes ----------

export function button(label, { icon: ic, kind = "", onClick, title, disabled, size } = {}) {
  return h("button", { class: `btn ${kind} ${size ?? ""}`.trim(), onclick: onClick, title, disabled }, ic && icon(ic, 16), label);
}

export function iconButton(name, label, onClick, cls = "") {
  return h("button", { class: `icon-btn ${cls}`.trim(), "aria-label": label, title: label, onclick: onClick }, icon(name, 17));
}

export function badge(text, color = "", ic) {
  return h("span", { class: `badge ${color}`.trim() }, ic && icon(ic, 12), text);
}

export function toggle(checked, onChange, label) {
  const input = h("input", { type: "checkbox", role: "switch", "aria-label": label });
  input.checked = checked;
  input.addEventListener("change", () => onChange?.(input.checked, input));
  return h("label", { class: "switch" }, input, h("span", { class: "track" }));
}

export function segmented(items, active, onChange) {
  const root = h("div", { class: "seg", role: "tablist" });
  const render = (current) => {
    clear(
      root,
      items.map((item) =>
        h(
          "button",
          {
            role: "tab",
            "aria-selected": String(item.id === current),
            onclick: () => {
              render(item.id);
              onChange(item.id);
            },
          },
          item.icon && icon(item.icon, 15),
          item.label,
          item.count != null && h("span", { class: "n" }, item.count),
        ),
      ),
    );
  };
  render(active);
  root.select = render;
  return root;
}

export function row({ icon: ic, lead, tint, title, subtitle, wrap, suffix, onClick, chevron, cls = "" }) {
  const tag = onClick ? "button" : "div";
  return h(
    tag,
    { class: `list-row ${cls}`.trim(), onclick: onClick },
    lead ?? (ic && h("span", { class: `lead ${tint ?? ""}`.trim() }, icon(ic, 18))),
    h("div", { class: "body" }, h("div", { class: "title" }, title), subtitle && h("div", { class: `subtitle ${wrap ? "wrap" : ""}` }, subtitle)),
    (suffix || chevron) && h("div", { class: "suffix" }, suffix, chevron && icon("chevron-right", 16, "ic chev")),
  );
}

export const list = (...rows) => h("div", { class: "list" }, rows);

export function section(title, { sub, actions, id } = {}, ...content) {
  return h(
    "section",
    { class: "section", id },
    h("div", { class: "section-head" }, h("h3", {}, title, sub && h("span", { class: "sub" }, sub)), actions && h("div", { class: "row-flex" }, actions)),
    content,
  );
}

export function hero(title, text, actions) {
  return h("div", { class: "page-hero" }, h("div", {}, h("h2", {}, title), text && h("p", {}, text)), actions && h("div", { class: "row-flex" }, actions));
}

export function meter(fraction, { warnAt = 0.8, critAt = 0.92, lg } = {}) {
  const cls = fraction >= critAt ? "crit" : fraction >= warnAt ? "warn" : "";
  const bar = h("i");
  bar.style.width = `${Math.max(0, Math.min(1, fraction)) * 100}%`;
  return h("div", { class: `meter ${cls} ${lg ? "lg" : ""}`.trim(), role: "progressbar", "aria-valuenow": Math.round(fraction * 100), "aria-valuemin": 0, "aria-valuemax": 100 }, bar);
}

export function banner(kind, ic, title, text, action) {
  return h("div", { class: `banner ${kind}` }, icon(ic, 22), h("div", { class: "body" }, h("strong", {}, title), text && h("span", {}, text)), action);
}

export function empty(ic, title, text, action, tone = "") {
  return h("div", { class: "empty" }, h("div", { class: `art ${tone}` }, icon(ic, 38)), h("h3", {}, title), text && h("p", {}, text), action);
}

export function skeletonRows(n = 4) {
  return list(
    Array.from({ length: n }, () =>
      h("div", { class: "list-row" }, h("div", { class: "skeleton", style: { width: "36px", height: "36px", "border-radius": "9px" } }), h("div", { class: "body" }, h("div", { class: "skeleton", style: { width: "42%", height: "13px", "margin-bottom": "8px" } }), h("div", { class: "skeleton", style: { width: "68%", height: "11px" } }))),
    ),
  );
}

const SVG_NS = "http://www.w3.org/2000/svg";
const s = (tag, attrs = {}) => {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
};

// Série temporal com área preenchida; `update(values)` redesenha sem recriar.
export function sparkline(cls = "spark", { max = 100, width = 200, height = 40, gridLines = 0 } = {}) {
  const root = s("svg", { class: cls, viewBox: `0 0 ${width} ${height}`, preserveAspectRatio: "none" });
  for (let i = 1; i <= gridLines; i++) {
    const y = (height * i) / (gridLines + 1);
    root.append(s("line", { class: "grid-line", x1: 0, x2: width, y1: y, y2: y }));
  }
  const area = s("path", { class: "area" });
  const line = s("path", { class: "line" });
  root.append(area, line);
  root.update = (values, top = max) => {
    if (values.length < 2) return;
    const peak = top ?? Math.max(...values, 1);
    const step = width / (values.length - 1);
    const pts = values.map((v, i) => [i * step, height - (Math.min(v, peak) / peak) * (height - 2) - 1]);
    const d = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join("");
    line.setAttribute("d", d);
    area.setAttribute("d", `${d}L${width},${height}L0,${height}Z`);
  };
  return root;
}

export function ring(fraction, { size = 64, stroke = 7, label } = {}) {
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const svgEl = s("svg", { width: size, height: size });
  const value = s("circle", { class: "value", cx: size / 2, cy: size / 2, r, fill: "none", "stroke-width": stroke, "stroke-linecap": "round", "stroke-dasharray": c });
  svgEl.append(s("circle", { class: "track", cx: size / 2, cy: size / 2, r, fill: "none", "stroke-width": stroke }), value);
  const text = h("span", { class: "label" });
  const root = h("div", { class: "ring" }, svgEl, text);
  root.style.width = `${size}px`;
  root.style.height = `${size}px`;
  root.update = (f, l) => {
    value.setAttribute("stroke-dashoffset", c * (1 - Math.max(0, Math.min(1, f))));
    root.classList.toggle("warn", f >= 0.85);
    text.textContent = l ?? `${Math.round(f * 100)}%`;
  };
  root.update(fraction, label);
  return root;
}

// Cor estável a partir de um nome, para tiles de apps e avatares.
export function hueFor(text) {
  let hash = 0;
  for (const ch of text) hash = (hash * 31 + ch.charCodeAt(0)) | 0;
  return `hsl(${Math.abs(hash) % 360} 58% 50%)`;
}

export function tile(name) {
  const el = h("span", { class: "app-tile" }, name.trim()[0]?.toUpperCase() ?? "?");
  el.style.background = `linear-gradient(150deg, ${hueFor(name)}, ${hueFor(name + "x")})`;
  return el;
}

// ---------- Sobreposições ----------

export function toast(message, { action, onAction, timeout = 4200, ic = "check-circle" } = {}) {
  const host = document.getElementById("toasts");
  let timer;
  const close = () => {
    clearTimeout(timer);
    el.classList.add("leaving");
    setTimeout(() => el.remove(), 200);
  };
  const el = h(
    "div",
    { class: "toast" },
    ic && icon(ic, 18, "t-ic"),
    h("span", {}, message),
    action && h("button", { class: "btn sm", onclick: () => { onAction?.(); close(); } }, action),
    h("button", { class: "icon-btn", "aria-label": tr("Close"), onclick: close }, icon("x", 15)),
  );
  host.append(el);
  while (host.children.length > 3) host.firstChild.remove();
  timer = setTimeout(close, timeout);
  return close;
}

let openLayers = [];
export function overlay(content, { top = false, onClose } = {}) {
  const previousFocus = document.activeElement;
  const root = h("div", { class: `overlay ${top ? "top" : ""}`.trim() }, content);
  const close = () => {
    root.remove();
    openLayers = openLayers.filter((l) => l !== layer);
    document.removeEventListener("keydown", onKey, true);
    onClose?.();
    previousFocus?.focus?.();
  };
  const layer = { close };
  const onKey = (e) => {
    if (openLayers.at(-1) !== layer) return;
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    } else if (e.key === "Tab") {
      const focusable = [...root.querySelectorAll("button, input, select, textarea, [tabindex]")].filter((el) => !el.disabled);
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable.at(-1);
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  };
  root.addEventListener("mousedown", (e) => { if (e.target === root) close(); });
  document.addEventListener("keydown", onKey, true);
  document.body.append(root);
  openLayers.push(layer);
  return close;
}

// Diálogo de confirmação no estilo AdwAlertDialog, com fatos que deixam claro
// o que vai acontecer (snapshot, reinício, reversibilidade) antes do clique.
//
// `required: false` segue a preferência "Confirmar ações administrativas":
// sem ela, o diálogo é pulado, exceto quando há formulário ou revisão
// (`extra`/`requireText`), como no vega-gtk.
export function confirm({ title, text, ic = "info", tone = "", facts = [], confirmLabel, confirmKind = "primary", extra, requireText, required = true }) {
  confirmLabel ??= tr("Continue");
  if (!required && !prefs.confirm_actions && !extra && !requireText) return Promise.resolve(true);
  return new Promise((resolve) => {
    let result = false;
    const ok = h("button", { class: `btn ${confirmKind}`, onclick: () => { result = true; close(); } }, confirmLabel);
    const typed = requireText && h("input", { class: "input", placeholder: requireText, "aria-label": trf("Type {text} to confirm", { text: requireText }) });
    if (typed) {
      ok.disabled = true;
      typed.style.width = "100%";
      typed.addEventListener("input", () => (ok.disabled = typed.value.trim() !== requireText));
    }
    const dialog = h(
      "div",
      { class: "dialog", role: "alertdialog", "aria-modal": "true", "aria-label": title },
      h(
        "div",
        { class: "dialog-body" },
        h("div", { class: `d-ic ${tone}`.trim() }, icon(ic, 26)),
        h("h2", {}, title),
        text && h("p", {}, text),
        (facts.length || extra || typed) &&
          h(
            "div",
            { class: "dialog-extra" },
            facts.length > 0 && h("ul", { class: "facts" }, facts.map((f) => h("li", { class: f.warn ? "warn" : "" }, icon(f.icon ?? "check", 16), h("span", {}, f.text)))),
            extra,
            typed && h("p", { class: "small muted", style: { margin: "14px 0 8px" } }, trf("To confirm, type {text}:", { text: requireText })),
            typed,
          ),
      ),
      h("div", { class: "dialog-actions" }, h("button", { class: "btn", onclick: () => close() }, tr("Cancel")), ok),
    );
    const close = overlay(dialog, { onClose: () => resolve(result) });
    requestAnimationFrame(() => (typed ?? ok).focus());
  });
}

export function sheet(title, body, { wide = true } = {}) {
  let close;
  const dialog = h(
    "div",
    { class: `dialog ${wide ? "wide" : ""}`, role: "dialog", "aria-modal": "true", "aria-label": title },
    h("div", { class: "sheet-head" }, h("h2", {}, title), h("button", { class: "icon-btn", "aria-label": tr("Close"), onclick: () => close() }, icon("x", 17))),
    h("div", { class: "sheet-body" }, body),
  );
  close = overlay(dialog);
  return close;
}

export function popover(anchor, content, cls = "") {
  document.querySelectorAll(".popover").forEach((p) => p.remove());
  const el = h("div", { class: `popover ${cls}`.trim(), role: "dialog" }, content);
  document.body.append(el);
  const rect = anchor.getBoundingClientRect();
  const width = el.offsetWidth;
  el.style.top = `${rect.bottom + 6}px`;
  el.style.left = `${Math.max(8, Math.min(window.innerWidth - width - 8, rect.right - width))}px`;
  const close = () => {
    el.remove();
    document.removeEventListener("mousedown", outside, true);
    document.removeEventListener("keydown", esc, true);
  };
  const outside = (e) => { if (!el.contains(e.target) && !anchor.contains(e.target)) close(); };
  const esc = (e) => { if (e.key === "Escape") { e.stopPropagation(); close(); anchor.focus(); } };
  setTimeout(() => document.addEventListener("mousedown", outside, true));
  document.addEventListener("keydown", esc, true);
  el.close = close;
  return el;
}

export function menuItem(ic, label, onClick, hint) {
  return h("button", { class: "menu-item", onclick: onClick }, icon(ic, 16), h("span", { class: "spacer" }, label), hint && h("kbd", {}, hint));
}

/** Toast de erro com o texto do backend. */
export function fail(error, prefix) {
  const message = error?.message ?? String(error);
  return toast(prefix ? `${prefix}: ${message}` : message, { ic: "alert-circle", timeout: 8000 });
}

/** Executa `fn` com o botão em estado ocupado (spinner + desabilitado). */
export async function busy(button, fn) {
  const previous = [...button.childNodes];
  button.disabled = true;
  button.replaceChildren(h("span", { class: "spinner" }), ...previous.filter((n) => n.nodeType === 3));
  try {
    return await fn();
  } finally {
    button.disabled = false;
    button.replaceChildren(...previous);
  }
}
