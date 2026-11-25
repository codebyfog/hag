mod editor;
mod files;
mod git_status;
mod palette;
mod terminal;
mod window;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

fn main() -> glib::ExitCode {
    glib::set_application_name("HAG");
    let app = adw::Application::new(Some("dev.victorsoares.HAG"), gio::ApplicationFlags::empty());
    app.connect_startup(|_| {
        if let Some(display) = gdk::Display::default() {
            let provider = gtk::CssProvider::new();
            provider.load_from_data(include_str!("../style.css"));
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );

            let icons = gtk::IconTheme::for_display(&display);
            let local_share = glib::user_data_dir().join("hag/icons");
            let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icons");
            icons.add_search_path(if assets.is_dir() { assets } else { local_share });
        }
    });
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            window::build(app, None);
        }
    });
    app.run()
}

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.set_action_name(Some(action));
    button.add_css_class("flat");
    button
}

fn close_on_escape(window: &adw::Window) {
    let controller = gtk::EventControllerKey::new();
    let window_ref = window.downgrade();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(window) = window_ref.upgrade() {
                window.close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(controller);
}
