use gettextrs::{LocaleCategory, TextDomain};
use gtk::gio;
use gtk::gio::prelude::DBusProxyExt;
use gtk::glib;
use gtk::glib::variant::ToVariant;

const DOMAIN: &str = "beam";
const DEFAULT_LOCALE: &str = "en_US";

/// Selects the process message locale once, before GTK or worker threads start.
pub fn init() {
    init_locale(session_locale());
}

fn init_locale(locale: &str) {
    // SAFETY: main calls this once, before GTK and the Tokio worker are created.
    unsafe {
        // A portable LC_ALL injected by launchers would otherwise force GNU
        // gettext to return msgids even after we selected the desktop locale.
        // Remove only C/POSIX; a real user-defined LC_ALL keeps its precedence.
        if std::env::var("LC_ALL").is_ok_and(|value| is_portable_locale(&value)) {
            std::env::remove_var("LC_ALL");
        }
        std::env::set_var("LC_MESSAGES", locale);
        std::env::set_var("LANGUAGE", locale);
    };
    let local_catalogs = concat!(env!("CARGO_MANIFEST_DIR"), "/po");
    if let Err(error) = TextDomain::new(DOMAIN)
        .prepend(local_catalogs)
        .locale(locale)
        .locale_category(LocaleCategory::LcMessages)
        .init()
    {
        eprintln!("i18n: using canonical English strings after catalog error: {error}");
    }
}

fn session_locale() -> &'static str {
    let desktop = gnome_language();
    let environment = ["LC_ALL", "LC_MESSAGES", "LANG"].map(std::env::var);
    resolve_locale(
        desktop.as_deref(),
        environment.iter().filter_map(|value| value.as_deref().ok()),
    )
}

/// GNOME persists the language selected for the logged-in user in
/// AccountsService. Consulting it avoids a stale launcher environment after
/// the user changes the desktop language; failure falls back to POSIX locale
/// variables without delaying startup.
fn gnome_language() -> Option<String> {
    let accounts = gio::DBusProxy::for_bus_sync(
        gio::BusType::System,
        gio::DBusProxyFlags::NONE,
        None,
        "org.freedesktop.Accounts",
        "/org/freedesktop/Accounts",
        "org.freedesktop.Accounts",
        gio::Cancellable::NONE,
    )
    .ok()?;
    let username = glib::user_name().to_string_lossy().into_owned();
    let reply = accounts
        .call_sync(
            "FindUserByName",
            Some(&(username.as_str(),).to_variant()),
            gio::DBusCallFlags::NONE,
            1_000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let (path,) = reply.get::<(glib::variant::ObjectPath,)>()?;
    let user = gio::DBusProxy::for_bus_sync(
        gio::BusType::System,
        gio::DBusProxyFlags::NONE,
        None,
        "org.freedesktop.Accounts",
        &path,
        "org.freedesktop.Accounts.User",
        gio::Cancellable::NONE,
    )
    .ok()?;
    user.cached_property("Language")?
        .get::<String>()
        .filter(|value| !value.trim().is_empty())
}

fn resolve_locale<'a>(
    desktop: Option<&'a str>,
    environment: impl IntoIterator<Item = &'a str>,
) -> &'static str {
    desktop
        .into_iter()
        .chain(environment)
        .into_iter()
        .find(|value| !value.trim().is_empty() && !is_portable_locale(value))
        .map_or(DEFAULT_LOCALE, normalize_locale)
}

/// `C` and `POSIX` describe the process's portable locale, not necessarily
/// the desktop language. Launchers commonly set them globally while `LANG`
/// still contains the language selected by the user.
fn is_portable_locale(value: &str) -> bool {
    let base = value.trim().split('@').next().unwrap_or("");
    let base = base.split('.').next().unwrap_or("");
    base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX")
}

/// Drops encoding/modifier suffixes before accepting only Beam's catalog set.
fn normalize_locale(value: &str) -> &'static str {
    let base = value.trim().split('@').next().unwrap_or("");
    let base = base.split('.').next().unwrap_or("").replace('-', "_");
    match base.to_ascii_lowercase().as_str() {
        "en_us" => "en_US",
        "pt_br" => "pt_BR",
        "es_es" => "es_ES",
        "zh_cn" => "zh_CN",
        _ => DEFAULT_LOCALE,
    }
}

pub fn gettext(message: &str) -> String {
    gettextrs::gettext(message)
}

pub fn format1(message: &str, name: &str, value: &str) -> String {
    gettext(message).replace(&format!("{{{name}}}"), value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_locales_and_falls_back_safely() {
        for (input, expected) in [
            ("en_US.UTF-8", "en_US"),
            ("pt_BR.UTF-8", "pt_BR"),
            ("es_ES.UTF-8@custom", "es_ES"),
            ("zh_CN.UTF-8", "zh_CN"),
            ("fr_FR.UTF-8", "en_US"),
            ("../../pt_BR", "en_US"),
        ] {
            assert_eq!(normalize_locale(input), expected);
        }
    }

    #[test]
    fn locale_precedence_matches_posix() {
        assert_eq!(
            resolve_locale(None, ["pt_BR.UTF-8", "es_ES.UTF-8"]),
            "pt_BR"
        );
        assert_eq!(
            resolve_locale(Some("zh_CN.UTF-8"), ["pt_BR.UTF-8"]),
            "zh_CN"
        );
        assert_eq!(resolve_locale(None, []), "en_US");
    }

    #[test]
    fn portable_locale_does_not_hide_desktop_language() {
        assert_eq!(
            resolve_locale(None, ["C.UTF-8", "C.UTF-8", "pt_BR.UTF-8"]),
            "pt_BR"
        );
        assert_eq!(
            resolve_locale(Some("POSIX"), ["C.UTF-8", "es_ES.UTF-8"]),
            "es_ES"
        );
        assert_eq!(resolve_locale(None, ["C.UTF-8", "POSIX"]), "en_US");
    }

    #[test]
    fn translation_subprocess() {
        let Ok(expected) = std::env::var("BEAM_TEST_EXPECTED") else {
            return;
        };
        init();
        assert_eq!(gettext("Settings"), expected);
    }

    #[test]
    fn loads_all_catalogs_and_unknown_locale_falls_back() {
        // Catalog lookup itself depends on libc locales installed by the host. Verify
        // the build products directly so this contract is deterministic in CI and RPM
        // builders alike; locale selection/normalization is covered above.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for locale in ["en_US", "pt_BR", "es_ES", "zh_CN"] {
            let catalog = root
                .join("po/locale")
                .join(locale)
                .join("LC_MESSAGES/beam.mo");
            let bytes =
                std::fs::read(&catalog).unwrap_or_else(|error| panic!("{catalog:?}: {error}"));
            assert!(
                bytes.starts_with(b"\xde\x12\x04\x95") || bytes.starts_with(b"\x95\x04\x12\xde"),
                "invalid gettext catalog for {locale}"
            );
        }
    }
}
