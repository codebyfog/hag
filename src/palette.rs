use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub fn show(parent: &adw::ApplicationWindow, root: &Path, open: impl Fn(&Path) + 'static) {
    let window = adw::Window::builder()
        .title("Find File")
        .default_width(580)
        .default_height(480)
        .modal(true)
        .transient_for(parent)
        .destroy_with_parent(true)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_bottom(16);
    let input = gtk::SearchEntry::new();
    input.set_placeholder_text(Some("Search by name or path…"));
    input.add_css_class("file-search");
    content.append(&input);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_valign(gtk::Align::Start);
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let spinner = gtk::Spinner::new();
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.start();
    stack.add_named(&spinner, Some("loading"));
    stack.add_named(&scroll, Some("results"));
    let empty = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .build();
    stack.add_named(&empty, Some("empty"));
    content.append(&stack);
    toolbar.set_content(Some(&content));
    window.set_content(Some(&toolbar));
    crate::close_on_escape(&window);

    let results = Rc::new(RefCell::new(Vec::new()));
    let files = Rc::new(RefCell::new(None::<Vec<PathBuf>>));
    let update = {
        let files = files.clone();
        let results = results.clone();
        let root = root.to_path_buf();
        let list = list.clone();
        let scroll = scroll.clone();
        move |query: &str| {
            let files = files.borrow();
            let Some(files) = files.as_ref() else { return };
            spinner.stop();
            let paths = matches(&root, files, query);
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            for path in &paths {
                list.append(&result_row(&root, path));
            }
            *results.borrow_mut() = paths;
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
                stack.set_visible_child_name("results");
                scroll.vadjustment().set_value(0.0);
            } else {
                empty.set_title(if files.is_empty() {
                    "No files in this project"
                } else {
                    "No matching files"
                });
                empty.set_description(if files.is_empty() {
                    None
                } else {
                    Some("Try a different name or path.")
                });
                stack.set_visible_child_name("empty");
            }
        }
    };
    let on_change = update.clone();
    input.connect_changed(move |entry| on_change(&entry.text()));
    let selection = list.clone();
    input.connect_activate(move |_| {
        if let Some(row) = selection.selected_row() {
            selection.emit_by_name::<()>("row-activated", &[&row]);
        }
    });

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let selection = list.clone();
    key.connect_key_pressed(move |_, key, _, _| {
        let offset = match key {
            gdk::Key::Up => -1,
            gdk::Key::Down => 1,
            _ => return glib::Propagation::Proceed,
        };
        let index = selection.selected_row().map_or(-1, |row| row.index());
        if let Some(row) = selection.row_at_index((index + offset).max(0)) {
            selection.select_row(Some(&row));
            if let Some(bounds) = row.compute_bounds(&selection) {
                scroll
                    .vadjustment()
                    .clamp_page(bounds.y() as f64, (bounds.y() + bounds.height()) as f64);
            }
        }
        glib::Propagation::Stop
    });
    input.add_controller(key);
    let dialog = window.downgrade();
    list.connect_row_activated(move |_, row| {
        if let Some(path) = results.borrow().get(row.index() as usize).cloned() {
            if let Some(window) = dialog.upgrade() {
                window.close();
            }
            open(&path);
        }
    });
    let project = root.to_owned();
    let weak = input.downgrade();
    glib::spawn_future_local(async move {
        let scanned = gio::spawn_blocking(move || crate::files::index(&project)).await;
        if let Some(input) = weak.upgrade() {
            *files.borrow_mut() = Some(scanned.unwrap_or_default());
            update(&input.text());
        }
    });
    window.present();
    input.grab_focus();
}

fn matches(root: &Path, files: &[PathBuf], query: &str) -> Vec<PathBuf> {
    let query = query.trim().to_lowercase();
    let mut ranked: Vec<_> = files
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .to_lowercase();
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            let rank = if query.is_empty() || name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if name.contains(&query) {
                2
            } else if relative.contains(&query) {
                3
            } else {
                return None;
            };
            Some((rank, relative, path))
        })
        .collect();
    ranked.sort_unstable();
    ranked
        .into_iter()
        .take(50)
        .map(|(_, _, path)| path.clone())
        .collect()
}

fn result_row(root: &Path, path: &Path) -> adw::ActionRow {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let directory = relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(root.file_name().unwrap_or(root.as_os_str())))
        .to_string_lossy();
    let row = adw::ActionRow::new();
    row.set_activatable(true);
    row.set_title(&glib::markup_escape_text(&name));
    row.set_subtitle(&glib::markup_escape_text(&directory));
    row.set_title_lines(1);
    row.set_subtitle_lines(1);
    row.set_tooltip_text(Some(&relative.to_string_lossy()));
    let image = gtk::Image::new();
    crate::files::set_file_icon(&image, path);
    row.add_prefix(&image);
    row
}
