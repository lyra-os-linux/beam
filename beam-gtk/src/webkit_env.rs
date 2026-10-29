//! Contorno do crash do WebKitGTK 2.52 ao encerrar (beam#7, vega#166).
//!
//! O renderer DMA-BUF do WebKit guarda o dispositivo GBM num singleton
//! estático (`WebCore::GBMDevice`). No `exit()` do WebKitWebProcess, o
//! destrutor dele corre contra o desmonte do Mesa e do glvnd e corrompe o
//! heap ("corrupted double-linked list"), deixando um coredump. Com o
//! renderer desligado, o compositing acelerado continua ligado. O Beam não
//! abre outros aplicativos, então nenhum processo filho herda a variável.
//! Retirar quando o WebKit corrigir.

use std::ffi::OsStr;

pub const DISABLE_DMABUF: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// Desliga o renderer DMA-BUF do WebKit. Chamar no início do `main`, antes
/// de qualquer thread. Um valor já definido pelo usuário é respeitado.
pub fn apply() {
    if should_set(std::env::var_os(DISABLE_DMABUF).as_deref()) {
        // SAFETY: chamado no início do main, com uma única thread.
        unsafe { std::env::set_var(DISABLE_DMABUF, "1") };
    }
}

fn should_set(existing: Option<&OsStr>) -> bool {
    existing.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_only_when_the_user_did_not() {
        assert!(should_set(None));
        assert!(!should_set(Some(OsStr::new("0"))));
        assert!(!should_set(Some(OsStr::new("1"))));
    }
}
