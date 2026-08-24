//! Main window: searchable list of saved connection profiles.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use beam_core::profile::{self, ConnectionProfile};
use gtk::gio;
use gtk::glib;
use gtk::glib::clone;

use crate::i18n::gettext;
use crate::{profile_dialog, settings, window_session};

pub fn build(app: &adw::Application, runtime: tokio::runtime::Handle) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Beam")
        .default_width(560)
        .default_height(640)
        .build();

    let (loaded_profiles, load_error) = match profile::load_profiles() {
        Ok(profiles) => (profiles, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let profiles: Rc<RefCell<Vec<ConnectionProfile>>> = Rc::new(RefCell::new(loaded_profiles));

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_btn.set_tooltip_text(Some(&gettext("New connection")));
    add_btn.update_property(&[gtk::accessible::Property::Label(&gettext("New connection"))]);
    header.pack_start(&add_btn);

    let menu = gio::Menu::new();
    let settings_section = gio::Menu::new();
    settings_section.append(Some(&gettext("Settings")), Some("win.settings"));
    menu.append_section(None, &settings_section);
    let about_section = gio::Menu::new();
    about_section.append(Some(&gettext("About Beam")), Some("win.about"));
    menu.append_section(None, &about_section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text(gettext("Main menu"))
        .menu_model(&menu)
        .build();
    menu_button.update_property(&[gtk::accessible::Property::Label(&gettext("Main menu"))]);
    header.pack_end(&menu_button);

    toolbar_view.add_top_bar(&header);

    install_window_actions(&window);

    let search_entry = gtk::SearchEntry::builder()
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .build();

    let list_store = gio::ListStore::new::<ConnectionProfileObject>();
    for p in profiles.borrow().iter() {
        list_store.append(&ConnectionProfileObject::new(p.clone()));
    }

    let filter = gtk::CustomFilter::new(clone!(
        #[weak]
        search_entry,
        #[upgrade_or]
        true,
        move |item| {
            let query = search_entry.text().to_lowercase();
            if query.is_empty() {
                return true;
            }
            let obj = item
                .downcast_ref::<ConnectionProfileObject>()
                .expect("ConnectionProfileObject");
            let p = obj.profile();
            p.name.to_lowercase().contains(&query) || p.host.to_lowercase().contains(&query)
        }
    ));
    let filter_model = gtk::FilterListModel::new(Some(list_store.clone()), Some(filter.clone()));
    search_entry.connect_search_changed(move |_| filter.changed(gtk::FilterChange::Different));

    let selection_model = gtk::NoSelection::new(Some(filter_model));

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, list_item| {
        let row = adw::ActionRow::new();
        let connect_icon = gtk::Image::from_icon_name("network-server-symbolic");
        connect_icon.set_tooltip_text(Some(&gettext("Connect")));
        connect_icon.update_property(&[gtk::accessible::Property::Label(&gettext("Connect"))]);
        row.add_prefix(&connect_icon);

        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        menu_btn.set_tooltip_text(Some(&gettext("Connection actions")));
        menu_btn.update_property(&[gtk::accessible::Property::Label(&gettext(
            "Connection actions",
        ))]);
        row.add_suffix(&menu_btn);
        row.set_activatable(true);

        list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem")
            .set_child(Some(&row));
    });

    factory.connect_bind(clone!(
        #[strong]
        profiles,
        #[strong]
        list_store,
        #[weak]
        window,
        move |_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let obj = list_item
                .item()
                .and_downcast::<ConnectionProfileObject>()
                .expect("item");
            let row = list_item
                .child()
                .and_downcast::<adw::ActionRow>()
                .expect("row");
            let p = obj.profile();
            row.set_title(&glib::markup_escape_text(&p.name));
            row.set_subtitle(&glib::markup_escape_text(&format!(
                "{}@{}",
                p.username,
                p.address()
            )));

            let menu_btn = row
                .last_child()
                .and_then(|w| w.prev_sibling())
                .and_downcast::<gtk::MenuButton>();
            if let Some(menu_btn) = menu_btn {
                let menu = gio::Menu::new();
                menu.append(Some(&gettext("Edit")), Some("row.edit"));
                menu.append(Some(&gettext("Duplicate")), Some("row.duplicate"));
                menu.append(Some(&gettext("Delete")), Some("row.delete"));
                let popover = gtk::PopoverMenu::from_model(Some(&menu));
                menu_btn.set_popover(Some(&popover));

                let actions = gio::SimpleActionGroup::new();
                let edit_action = gio::SimpleAction::new("edit", None);
                edit_action.connect_activate(clone!(
                    #[strong]
                    profiles,
                    #[strong]
                    list_store,
                    #[weak]
                    window,
                    #[strong]
                    obj,
                    move |_, _| {
                        let profiles = profiles.clone();
                        let list_store = list_store.clone();
                        let window = window.clone();
                        let current = obj.profile();
                        glib::MainContext::default().spawn_local(async move {
                            if let Some(updated) =
                                profile_dialog::edit(&window, Some(current)).await
                            {
                                let mut list = profiles.borrow_mut();
                                let Some(position) = list.iter().position(|p| p.id == updated.id)
                                else {
                                    return;
                                };
                                let previous = list[position].clone();
                                let mut saved = list.clone();
                                saved[position] = updated;
                                if let Err(error) = profile::save_profiles(&saved) {
                                    tracing::error!(%error, "falha ao salvar edição de perfil");
                                    show_error(&window, &error.to_string());
                                    return;
                                }
                                let remove_old_credential = !same_credential(&previous, &saved[position])
                                    && !saved.iter().any(|p| same_credential(p, &previous));
                                *list = saved;
                                drop(list);
                                refresh_store(&profiles, &list_store);
                                if remove_old_credential {
                                    let key = beam_core::secrets::SecretKey {
                                        host: previous.normalized_host(),
                                        port: previous.port,
                                        user: &previous.username,
                                    };
                                    if let Err(error) = beam_core::secrets::delete_password(&key).await {
                                        tracing::warn!(%error, "falha ao remover credencial antiga");
                                    }
                                }
                            }
                        });
                    }
                ));
                let duplicate_action = gio::SimpleAction::new("duplicate", None);
                duplicate_action.connect_activate(clone!(
                    #[strong]
                    profiles,
                    #[strong]
                    list_store,
                    #[strong]
                    obj,
                    #[weak]
                    window,
                    move |_, _| {
                        let mut list = profiles.borrow_mut();
                        let mut saved = list.clone();
                        saved.push(obj.profile().duplicate(&gettext("copy")));
                        if let Err(error) = profile::save_profiles(&saved) {
                            show_error(&window, &error.to_string());
                            return;
                        }
                        *list = saved;
                        drop(list);
                        refresh_store(&profiles, &list_store);
                    }
                ));
                let delete_action = gio::SimpleAction::new("delete", None);
                delete_action.connect_activate(clone!(
                    #[strong]
                    profiles,
                    #[strong]
                    list_store,
                    #[strong]
                    obj,
                    #[weak]
                    window,
                    move |_, _| {
                        let target = obj.profile();
                        let mut list = profiles.borrow_mut();
                        let mut updated = list.clone();
                        updated.retain(|p| p.id != target.id);
                        if let Err(error) = profile::save_profiles(&updated) {
                            tracing::error!(%error, "falha ao salvar exclusão de perfil");
                            show_error(&window, &error.to_string());
                            return;
                        }
                        let credential_still_used = updated.iter().any(|p| {
                            p.normalized_host() == target.normalized_host()
                                && p.port == target.port
                                && p.username == target.username
                        });
                        *list = updated;
                        drop(list);
                        refresh_store(&profiles, &list_store);
                        if !credential_still_used {
                            glib::MainContext::default().spawn_local(async move {
                                let key = beam_core::secrets::SecretKey {
                                    host: target.normalized_host(),
                                    port: target.port,
                                    user: &target.username,
                                };
                                if let Err(error) = beam_core::secrets::delete_password(&key).await
                                {
                                    tracing::warn!(%error, "falha ao remover credencial sem uso");
                                }
                            });
                        }
                    }
                ));
                actions.add_action(&edit_action);
                actions.add_action(&duplicate_action);
                actions.add_action(&delete_action);
                row.insert_action_group("row", Some(&actions));
            }
        }
    ));

    let list_view = gtk::ListView::new(Some(selection_model), Some(factory));
    list_view.set_single_click_activate(true);
    list_view.connect_activate(clone!(
        #[weak]
        window,
        #[strong]
        runtime,
        move |view, position| {
            let Some(obj) = view
                .model()
                .and_then(|model| model.item(position))
                .and_downcast::<ConnectionProfileObject>()
            else {
                return;
            };
            window_session::open(
                window
                    .application()
                    .and_downcast::<adw::Application>()
                    .as_ref()
                    .expect("app"),
                obj.profile(),
                runtime.clone(),
            );
        }
    ));

    let scroller = gtk::ScrolledWindow::builder()
        .child(&list_view)
        .vexpand(true)
        .build();

    let status_page = adw::StatusPage::builder()
        .title(gettext("No connections"))
        .description(gettext("Create your first connection to get started"))
        .icon_name("network-server-symbolic")
        .vexpand(true)
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&status_page, Some("empty"));
    stack.add_named(&scroller, Some("list"));
    update_stack(&stack, &list_store);
    list_store.connect_items_changed(clone!(
        #[weak]
        stack,
        move |store, _, _, _| update_stack(&stack, store)
    ));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&search_entry);
    content.append(&stack);
    toolbar_view.set_content(Some(&content));
    window.set_content(Some(&toolbar_view));

    add_btn.connect_clicked(clone!(
        #[weak]
        window,
        #[strong]
        profiles,
        #[strong]
        list_store,
        #[strong]
        runtime,
        move |_| {
            let profiles = profiles.clone();
            let list_store = list_store.clone();
            let window = window.clone();
            let _ = &runtime;
            glib::MainContext::default().spawn_local(async move {
                if let Some(new_profile) = profile_dialog::edit(&window, None).await {
                    let mut list = profiles.borrow_mut();
                    let mut saved = list.clone();
                    saved.push(new_profile);
                    if let Err(error) = profile::save_profiles(&saved) {
                        show_error(&window, &error.to_string());
                        return;
                    }
                    *list = saved;
                    drop(list);
                    refresh_store(&profiles, &list_store);
                }
            });
        }
    ));

    window.present();
    if let Some(error) = load_error {
        show_error(&window, &error);
    }
}

fn show_error(parent: &adw::ApplicationWindow, detail: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Could not save connections"))
        .body(detail)
        .build();
    dialog.add_response("close", &gettext("Close"));
    dialog.present(Some(parent));
}

fn install_window_actions(window: &adw::ApplicationWindow) {
    let settings_action = gio::SimpleAction::new("settings", None);
    settings_action.connect_activate(clone!(
        #[weak]
        window,
        move |_, _| settings::show(&window)
    ));
    window.add_action(&settings_action);

    let about_action = gio::SimpleAction::new("about", None);
    about_action.connect_activate(clone!(
        #[weak]
        window,
        move |_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Beam")
                .application_icon("org.lyraos.Beam")
                .developer_name("Lyra OS")
                .version(env!("CARGO_PKG_VERSION"))
                .website("https://github.com/lyra-os-linux/beam")
                .issue_url("https://github.com/lyra-os-linux/beam/issues")
                .license_type(gtk::License::Gpl30)
                .build();
            dialog.set_developers(&["Rodrigo Brito"]);
            dialog.present(Some(&window));
        }
    ));
    window.add_action(&about_action);
}

fn update_stack(stack: &gtk::Stack, store: &gio::ListStore) {
    stack.set_visible_child_name(if store.n_items() == 0 {
        "empty"
    } else {
        "list"
    });
}

fn refresh_store(profiles: &Rc<RefCell<Vec<ConnectionProfile>>>, store: &gio::ListStore) {
    store.remove_all();
    for p in profiles.borrow().iter() {
        store.append(&ConnectionProfileObject::new(p.clone()));
    }
}

fn same_credential(a: &ConnectionProfile, b: &ConnectionProfile) -> bool {
    a.normalized_host() == b.normalized_host() && a.port == b.port && a.username == b.username
}

glib::wrapper! {
    pub struct ConnectionProfileObject(ObjectSubclass<imp::ConnectionProfileObject>);
}

impl ConnectionProfileObject {
    pub fn new(profile: ConnectionProfile) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().profile.replace(Some(profile));
        obj
    }

    pub fn profile(&self) -> ConnectionProfile {
        self.imp()
            .profile
            .borrow()
            .clone()
            .expect("profile set at construction")
    }
}

mod imp {
    use std::cell::RefCell;

    use beam_core::profile::ConnectionProfile;
    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct ConnectionProfileObject {
        pub profile: RefCell<Option<ConnectionProfile>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConnectionProfileObject {
        const NAME: &'static str = "BeamConnectionProfileObject";
        type Type = super::ConnectionProfileObject;
    }

    impl ObjectImpl for ConnectionProfileObject {}
}

#[cfg(test)]
mod tests {
    use super::same_credential;
    use beam_core::profile::ConnectionProfile;

    #[test]
    fn duplicated_profiles_share_the_same_credential_identity() {
        let profile = ConnectionProfile::new("server", "[2001:db8::1]", "alice");
        let duplicate = profile.duplicate("copy");
        assert!(same_credential(&profile, &duplicate));
    }

    #[test]
    fn editing_endpoint_or_user_changes_credential_identity() {
        let profile = ConnectionProfile::new("server", "host", "alice");
        let mut edited = profile.clone();
        edited.username = "bob".into();
        assert!(!same_credential(&profile, &edited));
        edited.username = profile.username.clone();
        edited.port += 1;
        assert!(!same_credential(&profile, &edited));
    }
}
