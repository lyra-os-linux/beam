// Carregado antes dos módulos: registra no stderr erros que impediriam a
// interface de iniciar (falha de import, exceção no topo de um módulo).
(() => {
  const log = (message) => window.__TAURI__?.core.invoke("app_log", { message }).catch(() => {});
  window.addEventListener("error", (e) => log(`${e.message} @ ${(e.filename || "").split("/").pop()}:${e.lineno}`), true);
  window.addEventListener("unhandledrejection", (e) => log(`promise: ${e.reason?.stack ?? e.reason}`));
})();
