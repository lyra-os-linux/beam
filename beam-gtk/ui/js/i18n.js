// Tradução: os textos da interface são os mesmos msgids em inglês do domínio
// gettext `beam` (beam-gtk/po/*.po). O Rust entrega o catálogo do locale
// ativo uma vez; `tr()` é síncrono. tests/test-i18n.sh confere as chamadas
// tr()/trf() deste diretório, sempre com um literal entre aspas duplas.

let catalog = {};
export let locale = "en-US";

export async function loadCatalog(invoke, activeLocale) {
  locale = (activeLocale || "en_US").replace("_", "-");
  document.documentElement.lang = locale;
  try {
    catalog = (await invoke("i18n_catalog")) ?? {};
  } catch {
    catalog = {};
  }
}

export const tr = (msgid) => catalog[msgid] || msgid;

/** Traduz e troca `{nome}` pelos valores de `vars`. */
export function trf(msgid, vars = {}) {
  let text = tr(msgid);
  for (const [key, value] of Object.entries(vars)) text = text.replaceAll(`{${key}}`, String(value));
  return text;
}

/** Compatibilidade com ui.js (copiado do Vega): `t(msgid, vars)`. */
export const t = (msgid, vars) => (vars ? trf(msgid, vars) : tr(msgid));
