//! The session window: full desktop view, floating auto-hiding header in fullscreen, and a
//! reconnect banner when the connection drops.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use beam_core::profile::ConnectionProfile;
use beam_core::session::{self, SessionController, SessionEvents};
use gtk::gdk;
use gtk::glib;
use gtk::glib::clone;

use crate::display::RemoteDisplay;
use crate::i18n::gettext;
use crate::{cert_dialog, input_gtk, password_dialog};

const HEADER_REVEAL_ZONE_PX: f64 = 36.0;
const HEADER_AUTO_HIDE_MS: u32 = 1500;

pub fn open(app: &adw::Application, profile: ConnectionProfile, runtime: tokio::runtime::Handle) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(format!("{} — Beam", profile.name))
        .default_width(1152)
        .default_height(768)
        .build();

    let header = adw::HeaderBar::new();
    header.add_css_class("osd");
    header.set_title_widget(Some(&adw::WindowTitle::new(
        &profile.name,
        &profile.address(),
    )));

    let fullscreen_btn = gtk::Button::from_icon_name("view-fullscreen-symbolic");
    fullscreen_btn.set_tooltip_text(Some(&gettext("Fullscreen")));
    fullscreen_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Fullscreen"))]);
    header.pack_start(&fullscreen_btn);

    let cad_btn = gtk::Button::from_icon_name("input-keyboard-symbolic");
    cad_btn.set_tooltip_text(Some(&gettext("Send Ctrl+Alt+Del")));
    cad_btn.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Send Ctrl+Alt+Del",
    ))]);
    header.pack_start(&cad_btn);

    let disconnect_btn = gtk::Button::from_icon_name("network-offline-symbolic");
    disconnect_btn.set_tooltip_text(Some(&gettext("Disconnect")));
    disconnect_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Disconnect"))]);
    header.pack_end(&disconnect_btn);

    let header_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .reveal_child(true)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .child(&header)
        .build();

    let display = RemoteDisplay::new();
    display.set_hexpand(true);
    display.set_vexpand(true);
    display.set_focusable(true);
    display.set_can_focus(true);
    display.update_property(&[gtk::accessible::Property::Label(&gettext("Remote desktop"))]);

    let banner = adw::Banner::new(&gettext("Connection lost"));
    banner.set_button_label(Some(&gettext("Reconnect")));
    banner.set_revealed(false);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(&banner);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&display));
    overlay.add_overlay(&header_revealer);
    overlay.set_vexpand(true);
    content_box.append(&overlay);

    window.set_content(Some(&content_box));

    let fullscreen = Rc::new(Cell::new(false));
    window.connect_notify_local(
        Some("fullscreened"),
        clone!(
            #[strong]
            fullscreen,
            #[weak]
            fullscreen_btn,
            #[weak]
            header_revealer,
            move |window, _| {
                let active = window.is_fullscreen();
                fullscreen.set(active);
                fullscreen_btn.set_icon_name(if active {
                    "view-restore-symbolic"
                } else {
                    "view-fullscreen-symbolic"
                });
                fullscreen_btn.set_tooltip_text(Some(&gettext(if active {
                    "Leave fullscreen"
                } else {
                    "Fullscreen"
                })));
                header_revealer.set_reveal_child(!active);
            }
        ),
    );

    // Reveal the floating header when the pointer nears the top edge in fullscreen mode; leave
    // it permanently revealed in windowed mode.
    let hide_source: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
    {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[strong]
            fullscreen,
            #[weak]
            header_revealer,
            #[strong]
            hide_source,
            move |_, _x, y| {
                if !fullscreen.get() {
                    return;
                }
                if let Some(id) = hide_source.take() {
                    id.remove();
                }
                if y < HEADER_REVEAL_ZONE_PX {
                    header_revealer.set_reveal_child(true);
                } else {
                    let id = glib::timeout_add_local_once(
                        std::time::Duration::from_millis(u64::from(HEADER_AUTO_HIDE_MS)),
                        clone!(
                            #[weak]
                            header_revealer,
                            move || header_revealer.set_reveal_child(false)
                        ),
                    );
                    hide_source.set(Some(id));
                }
            }
        ));
        overlay.add_controller(motion);
    }

    let toggle_fullscreen = Rc::new(clone!(
        #[weak]
        window,
        #[weak]
        header_revealer,
        move || {
            let now_fullscreen = !window.is_fullscreen();
            if now_fullscreen {
                window.fullscreen();
                header_revealer.set_reveal_child(false);
            } else {
                window.unfullscreen();
                header_revealer.set_reveal_child(true);
            }
        }
    ));

    fullscreen_btn.connect_clicked(clone!(
        #[strong]
        toggle_fullscreen,
        move |_| toggle_fullscreen()
    ));

    // Capture toggle: with the pointer/keyboard "captured", events go to the remote session.
    // Ctrl+Alt+F12 always releases capture (and fullscreen) as a documented escape hatch;
    // clicking back into the display re-captures.
    let captured = Rc::new(Cell::new(true));

    let escape = gtk::EventControllerKey::new();
    escape.set_propagation_phase(gtk::PropagationPhase::Capture);
    escape.connect_key_pressed(clone!(
        #[strong]
        captured,
        #[strong]
        toggle_fullscreen,
        #[strong]
        fullscreen,
        move |_, keyval, _keycode, state| {
            if keyval == gdk::Key::F12
                && state.contains(gdk::ModifierType::CONTROL_MASK)
                && state.contains(gdk::ModifierType::ALT_MASK)
            {
                captured.set(false);
                if fullscreen.get() {
                    toggle_fullscreen();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(escape);

    let recapture = gtk::GestureClick::new();
    recapture.connect_pressed(clone!(
        #[strong]
        captured,
        #[weak]
        display,
        move |_, _, _, _| {
            captured.set(true);
            display.grab_focus();
        }
    ));
    display.add_controller(recapture);

    // Kick off the connection.
    let (controller, events) = session::connect(profile.clone(), &runtime);
    display.set_framebuffer(controller.framebuffer().clone());
    let active_controller = Rc::new(RefCell::new(controller.clone()));
    disconnect_on_close(&window, active_controller.clone());

    input_gtk::attach(
        &display,
        clone!(
            #[weak(rename_to = display)]
            display,
            #[upgrade_or]
            None,
            move |x, y| display.widget_to_remote(x, y)
        ),
        clone!(
            #[strong]
            active_controller,
            #[strong]
            captured,
            move |event| {
                if captured.get() {
                    active_controller.borrow().send_input(event);
                }
            }
        ),
    );

    cad_btn.connect_clicked(clone!(
        #[strong]
        active_controller,
        move |_| active_controller.borrow().send_ctrl_alt_del()
    ));
    disconnect_btn.connect_clicked(clone!(
        #[strong]
        active_controller,
        move |_| active_controller.borrow().disconnect()
    ));

    // Local → remote clipboard: forward text changes from the GTK clipboard.
    let clipboard = display.clipboard();
    clipboard.connect_changed(clone!(
        #[strong]
        active_controller,
        move |clipboard| {
            let controller = active_controller.borrow().clone();
            let clipboard = clipboard.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Ok(Some(text)) = clipboard.read_text_future().await {
                    controller.set_local_clipboard_text(text.to_string());
                }
            });
        }
    ));

    let profile_for_reconnect = profile.clone();
    let runtime_for_reconnect = runtime.clone();

    spawn_event_pump(
        events,
        controller.clone(),
        display.clone(),
        banner.clone(),
        window.clone(),
    );

    banner.connect_button_clicked(clone!(
        #[weak]
        window,
        #[weak]
        display,
        #[weak]
        banner,
        #[strong]
        active_controller,
        move |_| {
            banner.set_revealed(false);
            let (controller, events) =
                session::connect(profile_for_reconnect.clone(), &runtime_for_reconnect);
            display.set_framebuffer(controller.framebuffer().clone());
            *active_controller.borrow_mut() = controller.clone();
            spawn_event_pump(
                events,
                controller,
                display.clone(),
                banner.clone(),
                window.clone(),
            );
        }
    ));

    window.present();
    if should_start_fullscreen(&profile) {
        window.fullscreen();
    }
    display.grab_focus();
}

fn disconnect_on_close(
    window: &adw::ApplicationWindow,
    controller: Rc<RefCell<SessionController>>,
) {
    window.connect_close_request(move |_| {
        controller.borrow().disconnect();
        glib::Propagation::Proceed
    });
}

fn should_start_fullscreen(profile: &ConnectionProfile) -> bool {
    profile.fullscreen
}

fn spawn_event_pump(
    mut events: SessionEvents,
    controller: SessionController,
    display: RemoteDisplay,
    banner: adw::Banner,
    window: adw::ApplicationWindow,
) {
    glib::MainContext::default().spawn_local(async move {
        while let Some(event) = events.next_event().await {
            match event {
                beam_core::events::SessionEvent::Connected { .. } => {
                    banner.set_revealed(false);
                    display.mark_dirty();
                }
                beam_core::events::SessionEvent::FramebufferUpdated { .. } => {
                    display.mark_dirty();
                }
                beam_core::events::SessionEvent::CertPrompt(request) => {
                    let accepted = cert_dialog::confirm(
                        &window,
                        &request.address,
                        request.fingerprint.as_str(),
                        &request.decision,
                    )
                    .await;
                    let _ = request.respond.send(accepted);
                }
                beam_core::events::SessionEvent::CredsNeeded(request) => {
                    let answer = password_dialog::ask(&window, &request.username).await;
                    let _ = request.respond.send(answer);
                }
                beam_core::events::SessionEvent::ClipboardTextReceived(text) => {
                    display.clipboard().set_text(&text);
                }
                beam_core::events::SessionEvent::FeatureUnavailable { feature, detail } => {
                    banner.set_title(
                        &gettext("{feature} unavailable: {detail}")
                            .replace("{feature}", feature)
                            .replace("{detail}", &detail),
                    );
                    banner.set_revealed(true);
                }
                beam_core::events::SessionEvent::Disconnected(reason) => {
                    match reason {
                        beam_core::events::DisconnectReason::UserInitiated => {
                            window.close();
                        }
                        beam_core::events::DisconnectReason::ConnectionLost(detail) => {
                            banner.set_title(
                                &gettext("Connection lost: {detail}")
                                    .replace("{detail}", &localized_detail(&detail)),
                            );
                            banner.set_revealed(true);
                        }
                        beam_core::events::DisconnectReason::ConnectionFailed(detail) => {
                            banner.set_title(
                                &gettext("Connection failed: {detail}")
                                    .replace("{detail}", &localized_detail(&detail)),
                            );
                            banner.set_revealed(true);
                        }
                    }
                    break;
                }
            }
        }
        let _ = controller;
    });
}

/// Translate Beam-owned error text while retaining third-party/network details
/// verbatim so diagnostics are not damaged.
fn localized_detail(detail: &str) -> String {
    match detail {
        "Input queue full; disconnected to avoid losing key releases" => {
            gettext("Input queue full; disconnected to avoid losing key releases")
        }
        "Timed out sending data" => gettext("Timed out sending data"),
        "Timed out reactivating the session" => gettext("Timed out reactivating the session"),
        "Session ended unexpectedly" => gettext("Session ended unexpectedly"),
        "falha ao enviar dados" => gettext("Failed to send data"),
        "certificado do servidor recusado pelo usuário" => {
            gettext("Server certificate rejected by the user")
        }
        "conexão cancelada pelo usuário" => gettext("Connection cancelled by the user"),
        _ if detail.starts_with("falha de rede: ") => gettext("Network error: {detail}")
            .replace("{detail}", detail.trim_start_matches("falha de rede: ")),
        _ => detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::should_start_fullscreen;
    use beam_core::profile::ConnectionProfile;

    #[test]
    fn initial_presentation_follows_profile() {
        let mut profile = ConnectionProfile::new("test", "server", "user");
        profile.fullscreen = false;
        assert!(!should_start_fullscreen(&profile));
        profile.fullscreen = true;
        assert!(should_start_fullscreen(&profile));
    }
    #[test]
    #[ignore = "requires a private GTK display; run under Xvfb or headless Mutter"]
    fn closing_window_with_queued_input_keeps_main_context_responsive() {
        use super::*;
        use beam_core::events::{DisconnectReason, SessionEvent};
        use beam_core::session::InputEvent;
        adw::init().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // This runtime is deliberately not driven until after close: no keyring/network access.
        let (controller, mut events) = beam_core::session::connect(
            ConnectionProfile::new("test", "unused", "user"),
            runtime.handle(),
        );
        let controller = Rc::new(RefCell::new(controller));
        let window = adw::ApplicationWindow::builder().build();
        disconnect_on_close(&window, controller.clone());
        window.present();
        let ticked = Rc::new(std::cell::Cell::new(false));
        let ticked_callback = ticked.clone();
        glib::idle_add_local_once(move || {
            // More than the former bounded queue can hold, including matched key releases.
            for _ in 0..512 {
                for pressed in [true, false] {
                    controller.borrow().send_input(InputEvent::Key {
                        scancode: 30,
                        extended: false,
                        pressed,
                    });
                }
            }
            window.close();
            ticked_callback.set(true);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let context = glib::MainContext::default();
        while !ticked.get() && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        assert!(ticked.get(), "GTK callback stalled");
        runtime.block_on(async {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), events.next_event())
                    .await
                    .unwrap();
            assert!(matches!(
                event,
                Some(SessionEvent::Disconnected(DisconnectReason::UserInitiated))
            ));
            assert!(events.next_event().await.is_none());
        });
    }
}
