use std::path::Path;

use adw::prelude::*;
use gtk::{gio, glib, pango};
use vte::prelude::*;

fn configured_dark_theme() -> Option<bool> {
    let config = std::env::var_os("HELIX_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| glib::user_config_dir().join("helix"));
    let text = std::fs::read_to_string(config.join("config.toml")).ok()?;
    let settings = text.parse::<toml::Table>().ok()?;
    match settings.get("theme")?.as_str()? {
        "adwaita-dark" => Some(true),
        "adwaita-light" => Some(false),
        _ => None,
    }
}

pub fn widget() -> vte::Terminal {
    let terminal = vte::Terminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal.set_scrollback_lines(10_000);
    let settings = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup("org.gnome.desktop.interface", true))
        .map(|schema| gio::Settings::new_full(&schema, gio::SettingsBackend::NONE, None));
    if let Some(settings) = settings {
        let mut font = pango::FontDescription::from_string(&settings.string("monospace-font-name"));
        font.set_size(11 * pango::SCALE);
        terminal.set_font(Some(&font));
    }

    let dark = configured_dark_theme();
    let manager = adw::StyleManager::default();
    let theme_handler = manager.connect_dark_notify({
        let terminal = terminal.downgrade();
        move |_| {
            if let Some(terminal) = terminal.upgrade() {
                sync_background(&terminal, dark);
            }
        }
    });
    let theme_handler = std::cell::Cell::new(Some(theme_handler));
    terminal.connect_destroy(move |_| {
        if let Some(handler) = theme_handler.take() {
            manager.disconnect(handler);
        }
    });
    terminal.connect_realize(move |terminal| sync_background(terminal, dark));
    terminal
}

fn sync_background(terminal: &vte::Terminal, preference: Option<bool>) {
    let dark = preference.unwrap_or_else(|| adw::StyleManager::default().is_dark());
    // Default text and canvas colors from Helix's Adwaita runtime themes.
    let (background, foreground) = if dark {
        ("#1d1d1d", "#deddda")
    } else {
        ("#ffffff", "#3d3846")
    };
    terminal.set_clear_background(true);
    terminal.set_color_background(&gtk::gdk::RGBA::parse(background).expect("Adwaita background"));
    terminal.set_color_foreground(&gtk::gdk::RGBA::parse(foreground).expect("Adwaita foreground"));
    if let Some(parent) = terminal
        .parent()
        .filter(|parent| parent.has_css_class("editor-container"))
    {
        parent.remove_css_class(if dark { "light-canvas" } else { "dark-canvas" });
        parent.add_css_class(if dark { "dark-canvas" } else { "light-canvas" });
    }
}

pub fn spawn(
    terminal: &vte::Terminal,
    root: &Path,
    program: &str,
    done: impl FnOnce(Result<glib::Pid, glib::Error>) + 'static,
) {
    terminal.spawn_async(
        vte::PtyFlags::DEFAULT,
        root.to_str(),
        &[program],
        &["TERM=xterm-256color", "COLORTERM=truecolor"],
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        gio::Cancellable::NONE,
        done,
    );
}

pub fn git(parent: &adw::ApplicationWindow, root: &Path, toast: &adw::ToastOverlay) {
    if glib::find_program_in_path("lazygit").is_none() {
        toast.add_toast(adw::Toast::new("Install lazygit to open the Git panel."));
        return;
    }

    let window = adw::Window::builder()
        .title("Lazygit")
        .default_width(1050)
        .default_height(650)
        .transient_for(parent)
        .destroy_with_parent(true)
        .build();
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    view.add_top_bar(&header);
    let terminal = widget();
    terminal.add_css_class("git-terminal");
    view.set_content(Some(&terminal));
    window.set_content(Some(&view));
    crate::close_on_escape(&window);

    let weak = window.downgrade();
    terminal.connect_child_exited(move |_, _| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let toast = toast.downgrade();
    let weak = window.downgrade();
    spawn(&terminal, root, "lazygit", move |result| {
        if let Err(error) = result {
            if let Some(toast) = toast.upgrade() {
                toast.add_toast(adw::Toast::new(&format!("Lazygit: {error}")));
            }
            if let Some(window) = weak.upgrade() {
                window.close();
            }
        }
    });
    window.present();
    terminal.grab_focus();
}

pub struct Panel {
    pub widget: gtk::Box,
    tabs: adw::TabView,
}

impl Panel {
    pub fn new(toggle: &gtk::ToggleButton) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_size_request(-1, 240);
        widget.set_visible(false);

        let tabs = adw::TabView::new();
        tabs.set_vexpand(true);
        let bar = adw::TabBar::new();
        bar.set_autohide(false);
        bar.set_view(Some(&tabs));
        bar.add_css_class("terminal-panel-header");
        bar.set_end_action_widget(Some(&crate::icon_button(
            "list-add-symbolic",
            "New terminal",
            "win.new-terminal",
        )));
        widget.append(&bar);
        widget.append(&tabs);

        let toggle = toggle.downgrade();
        tabs.connect_n_pages_notify(move |tabs| {
            if tabs.n_pages() == 0 {
                if let Some(toggle) = toggle.upgrade() {
                    toggle.set_active(false);
                }
            }
        });
        Self { widget, tabs }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.n_pages() == 0
    }

    pub fn add(&self, root: &Path, toast: &adw::ToastOverlay) {
        let terminal = widget();
        let page = self.tabs.append(&terminal);
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        page.set_title(
            Path::new(&shell)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("Terminal"),
        );
        self.tabs.set_selected_page(&page);

        let close_page = {
            let tabs = self.tabs.downgrade();
            let page = page.downgrade();
            move || {
                if let (Some(tabs), Some(page)) = (tabs.upgrade(), page.upgrade()) {
                    if page.child().parent().is_some() {
                        tabs.close_page(&page);
                    }
                }
            }
        };
        let close_on_exit = close_page.clone();
        terminal.connect_child_exited(move |_, _| close_on_exit());
        let toast = toast.downgrade();
        spawn(&terminal, root, &shell, move |result| {
            if let Err(error) = result {
                if let Some(toast) = toast.upgrade() {
                    toast.add_toast(adw::Toast::new(&format!("Terminal: {error}")));
                }
                close_page();
            }
        });
        terminal.grab_focus();
    }
}
