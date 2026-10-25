//! Clipboard de texto da sessão, no GTK 3 que o Tauri usa no Linux. Toda
//! operação do GTK acontece na thread principal.

use gtk::glib::prelude::*;
use tauri::{AppHandle, Manager};

use crate::session::Sessions;

fn system_clipboard() -> gtk::Clipboard {
    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD)
}

/// Remoto → local: o texto copiado no Windows vai para o clipboard do GNOME.
pub fn set_text(app: &AppHandle, text: String) {
    let _ = app.run_on_main_thread(move || system_clipboard().set_text(&text));
}

/// Local → remoto: cada texto novo no clipboard do GNOME é oferecido às
/// sessões conectadas. Chamado no `setup`, já na thread principal.
pub fn watch(app: AppHandle) {
    // O gtk 0.18 não gera `connect_owner_change`; o sinal é ligado pelo nome.
    system_clipboard().connect_local("owner-change", false, move |args| {
        let clipboard = args.first()?.get::<gtk::Clipboard>().ok()?;
        let app = app.clone();
        clipboard.request_text(move |_, text| {
            if let Some(text) = text {
                app.state::<Sessions>().offer_clipboard(text);
            }
        });
        None
    });
}
