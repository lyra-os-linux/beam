// Ponte com o Rust (comandos Tauri e eventos).

const tauri = window.__TAURI__;

/** Chama um comando; erros chegam como texto já traduzido. */
export async function call(cmd, args = {}) {
  try {
    return await tauri.core.invoke(cmd, args);
  } catch (error) {
    throw new Error(typeof error === "string" ? error : error?.message ?? String(error));
  }
}

/** Escuta um evento do Rust; devolve a função que cancela a escuta. */
export function on(event, handler) {
  const pending = tauri.event.listen(event, (e) => handler(e.payload));
  return () => pending.then((unlisten) => unlisten());
}

/** Canal binário do Tauri (quadros da sessão). */
export const newChannel = () => new tauri.core.Channel();

export const win = tauri.window.getCurrentWindow();
