//! Integração com o GNOME (esquema de cor e cor de destaque) e comandos do
//! próprio aplicativo.

use gio::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

fn interface_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup("org.gnome.desktop.interface", true)?;
    Some(gio::Settings::new("org.gnome.desktop.interface"))
}

fn interface_state(settings: &gio::Settings) -> Value {
    let has_accent = settings
        .settings_schema()
        .is_some_and(|schema| schema.has_key("accent-color"));
    json!({
        "color_scheme": settings.string("color-scheme").to_string(),
        "accent": if has_accent { settings.string("accent-color").to_string() } else { "blue".to_owned() },
    })
}

#[tauri::command]
pub fn interface_style() -> Value {
    interface_settings()
        .map(|settings| interface_state(&settings))
        .unwrap_or_else(|| json!({ "color_scheme": "default", "accent": "blue" }))
}

/// Avisa todas as janelas quando o tema ou o destaque do GNOME mudam.
pub fn watch_interface(app: AppHandle) {
    let Some(settings) = interface_settings() else {
        return;
    };
    settings.connect_changed(None, move |settings, key| {
        if key == "color-scheme" || key == "accent-color" {
            let _ = app.emit("interface-changed", interface_state(settings));
        }
    });
    // GSettings só emite "changed" para chaves já lidas.
    let _ = interface_state(&settings);
    std::mem::forget(settings);
}

#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    locale: &'static str,
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        locale: crate::i18n::active_locale(),
    }
}

/// Erros que impediriam a interface de iniciar vão para o stderr.
#[tauri::command]
pub fn app_log(message: String) {
    eprintln!("beam (ui): {message}");
}
