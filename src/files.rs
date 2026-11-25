use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, OpenOptions},
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

const IGNORED: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    ".next",
    "dist",
    "build",
    ".cache",
    "target",
    ".venv",
    ".tox",
    "coverage",
];

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    name: String,
    directory: bool,
}

fn entries(root: &Path, sidebar: bool) -> Vec<Entry> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|item| {
            let name = item.file_name().to_string_lossy().into_owned();
            let directory = item.file_type().ok()?.is_dir();
            let hidden = name.starts_with('.');
            let excluded = hidden || IGNORED.contains(&name.as_str());
            if (sidebar && excluded && !matches!(name.as_str(), ".env" | ".envrc"))
                || (!sidebar && directory && excluded)
            {
                return None;
            }
            Some(Entry {
                path: item.path(),
                name,
                directory,
            })
        })
        .collect();
    entries.sort_by_cached_key(|entry| {
        (
            !entry.directory,
            entry.name.to_lowercase(),
            entry.path.clone(),
        )
    });
    entries
}

pub fn index(root: &Path) -> Vec<PathBuf> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in entries(&directory, false) {
            if entry.directory {
                directories.push(entry.path);
            } else if entry.path.is_file() {
                files.push(entry.path);
                if files.len() == 5000 {
                    return files;
                }
            }
        }
    }
    files
}

struct Directory {
    store: glib::WeakRef<gio::ListStore>,
    _monitor: Option<gio::FileMonitor>,
}

pub type OpenFile = Rc<dyn Fn(&Path)>;
pub type Notify = Rc<dyn Fn(&str)>;

#[derive(Default)]
struct Models(RefCell<HashMap<PathBuf, Directory>>);

impl Models {
    fn directory(&self, path: &Path) -> gio::ListStore {
        if let Some(store) = self
            .0
            .borrow()
            .get(path)
            .and_then(|directory| directory.store.upgrade())
        {
            return store;
        }
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        refresh(path, &store);
        let monitor = gio::File::for_path(path)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok();
        if let Some(monitor) = &monitor {
            let path = path.to_owned();
            let store = store.downgrade();
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(store) = store.upgrade() {
                    refresh(&path, &store);
                }
            });
        }
        let mut models = self.0.borrow_mut();
        models.retain(|_, directory| directory.store.upgrade().is_some());
        models.insert(
            path.to_owned(),
            Directory {
                store: store.downgrade(),
                _monitor: monitor,
            },
        );
        store
    }
}

fn refresh(root: &Path, store: &gio::ListStore) {
    let entries = entries(root, true);
    // Reuse rows for unchanged files so expanded subtrees stay attached.
    let mut position = 0;
    for entry in entries {
        loop {
            let Some(current) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                store.append(&glib::BoxedAnyObject::new(entry));
                break;
            };
            let current = current.borrow::<Entry>();
            if *current == entry {
                break;
            }
            let current_key = (
                !current.directory,
                current.name.to_lowercase(),
                &current.path,
            );
            let entry_key = (!entry.directory, entry.name.to_lowercase(), &entry.path);
            if current_key < entry_key {
                store.remove(position);
            } else {
                store.insert(position, &glib::BoxedAnyObject::new(entry));
                break;
            }
        }
        position += 1;
    }
    if position < store.n_items() {
        store.splice(
            position,
            store.n_items() - position,
            &[] as &[glib::BoxedAnyObject],
        );
    }
}

pub struct Tree {
    pub widget: gtk::Box,
    root: PathBuf,
    model: gtk::TreeListModel,
    highlight: Rc<RefCell<Highlight>>,
}

#[derive(Default)]
struct Highlight {
    active: Option<PathBuf>,
    statuses: HashMap<PathBuf, crate::git_status::Status>,
    rows: HashMap<PathBuf, glib::WeakRef<gtk::TreeExpander>>,
}

impl Tree {
    pub fn new(
        root: &Path,
        parent: &adw::ApplicationWindow,
        open: OpenFile,
        notify: Notify,
    ) -> Self {
        let models = Rc::new(Models::default());
        let model = gtk::TreeListModel::new(models.directory(root), false, false, move |object| {
            let entry = object
                .downcast_ref::<glib::BoxedAnyObject>()?
                .borrow::<Entry>();
            entry
                .directory
                .then(|| models.directory(&entry.path).upcast())
        });
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = gtk::SignalListItemFactory::new();
        let parent = parent.downgrade();
        let row_open = open.clone();
        let row_notify = notify.clone();
        let project_root = root.to_owned();
        factory.connect_setup(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
            let expander = gtk::TreeExpander::new();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let icon = gtk::Image::new();
            icon.set_pixel_size(16);
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(pango::EllipsizeMode::Middle);
            label.set_has_tooltip(true);
            label.connect_query_tooltip(|label, _, _, _, tooltip| {
                if label.layout().is_ellipsized() {
                    tooltip.set_text(Some(&label.text()));
                    true
                } else {
                    false
                }
            });
            content.append(&icon);
            content.append(&label);
            let badge = gtk::Label::new(None);
            badge.add_css_class("caption");
            badge.add_css_class("git-status");
            badge.set_valign(gtk::Align::Center);
            badge.set_width_chars(2);
            badge.set_margin_end(10);
            badge.set_xalign(0.5);
            badge.set_visible(false);
            content.append(&badge);
            expander.set_child(Some(&content));
            item.set_child(Some(&expander));

            let source = gtk::DragSource::new();
            source.set_actions(gdk::DragAction::MOVE);
            source.set_propagation_phase(gtk::PropagationPhase::Capture);
            let row = expander.downgrade();
            source.connect_prepare(move |_, _, _| {
                let entry = entry_for(row.upgrade()?.list_row()?)?;
                Some(gdk::ContentProvider::for_value(
                    &glib::BoxedAnyObject::new(entry).to_value(),
                ))
            });
            source.connect_drag_begin(move |_, drag| {
                let preview = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                preview.add_css_class("file-drag-preview");
                let symbol = gtk::Image::new();
                if let Some(name) = icon.icon_name() {
                    symbol.set_icon_name(Some(&name));
                } else if let Some(gicon) = icon.gicon() {
                    symbol.set_from_gicon(&gicon);
                }
                symbol.set_pixel_size(16);
                let title = gtk::Label::new(Some(&label.text()));
                title.set_single_line_mode(true);
                title.set_ellipsize(pango::EllipsizeMode::Middle);
                title.set_max_width_chars(32);
                preview.append(&symbol);
                preview.append(&title);
                gtk::DragIcon::for_drag(drag).set_child(Some(&preview));
                drag.set_hotspot(0, 0);
            });
            expander.add_controller(source);
            let row = expander.downgrade();
            install_move_target(&expander, &project_root, &row_notify, move || {
                let entry = entry_for(row.upgrade()?.list_row()?)?;
                entry.directory.then_some(entry.path)
            });

            let click = gtk::GestureClick::new();
            click.set_button(3);
            let expander_ref = expander.downgrade();
            let parent = parent.clone();
            let open = row_open.clone();
            let notify = row_notify.clone();
            click.connect_pressed(move |gesture, _, _, _| {
                let (Some(expander), Some(parent)) = (expander_ref.upgrade(), parent.upgrade())
                else {
                    return;
                };
                if let Some(entry) = expander.list_row().and_then(entry_for) {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    context_menu(&expander, &parent, entry, open.clone(), notify.clone());
                }
            });
            expander.add_controller(click);
        });
        let highlight = Rc::new(RefCell::new(Highlight::default()));
        let binding = highlight.clone();
        factory.connect_bind(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
            let row = item
                .item()
                .and_downcast::<gtk::TreeListRow>()
                .expect("tree row");
            let expander = item
                .child()
                .and_downcast::<gtk::TreeExpander>()
                .expect("expander");
            expander.set_list_row(Some(&row));
            if let Some(entry) = entry_for(row) {
                let mut state = binding.borrow_mut();
                if state.active.as_deref() == Some(&entry.path) {
                    expander.add_css_class("active-file");
                }
                state.rows.insert(entry.path.clone(), expander.downgrade());
                set_badge(&expander, state.statuses.get(&entry.path).copied());
                drop(state);
                let content = expander.child().expect("row content");
                let image = content
                    .first_child()
                    .and_downcast::<gtk::Image>()
                    .expect("icon");
                let label = image
                    .next_sibling()
                    .and_downcast::<gtk::Label>()
                    .expect("label");
                label.set_text(&entry.name);
                if entry.directory {
                    image.set_icon_name(Some("folder-symbolic"));
                } else {
                    set_file_icon(&image, &entry.path);
                }
            }
        });

        let binding = highlight.clone();
        factory.connect_unbind(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
            let expander = item
                .child()
                .and_downcast::<gtk::TreeExpander>()
                .expect("expander");
            binding
                .borrow_mut()
                .rows
                .retain(|_, widget| widget.upgrade().is_some_and(|widget| widget != expander));
            expander.remove_css_class("active-file");
            expander.remove_css_class("context-target");
            expander.set_list_row(None);
        });

        let list = gtk::ListView::new(Some(selection), Some(factory));
        list.set_single_click_activate(true);
        list.add_css_class("navigation-sidebar");
        let rows = model.clone();
        list.connect_activate(move |_, position| {
            if let Some(row) = rows.row(position) {
                if let Some(entry) = entry_for(row.clone()) {
                    if entry.directory {
                        row.set_expanded(!row.is_expanded());
                    } else {
                        open(&entry.path);
                    }
                }
            }
        });

        let header = adw::HeaderBar::new();
        header.set_show_title(false);
        header.set_show_end_title_buttons(false);
        header.pack_start(&crate::icon_button(
            "system-search-symbolic",
            "Find file",
            "win.open-palette",
        ));
        header.pack_end(&crate::icon_button(
            "builder-vcs-branch-symbolic",
            "Git",
            "win.open-git",
        ));
        let project = gtk::Label::new(
            root.file_name()
                .map(|name| name.to_string_lossy())
                .as_deref(),
        );
        project.add_css_class("section-header-label");
        let root_path = root.to_owned();
        install_move_target(&project, root, &notify, move || Some(root_path.clone()));
        project.set_tooltip_text(Some("Drop here to move to the project root"));
        project.set_xalign(0.0);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("helix-sidebar");
        widget.set_size_request(220, -1);
        widget.append(&header);
        widget.append(&project);
        widget.append(&scroll);
        watch_status(root, &widget, &highlight);
        Self {
            widget,
            root: root.to_owned(),
            model,
            highlight,
        }
    }

    pub fn highlight(&self, path: Option<&Path>) {
        let path = path.filter(|path| path.starts_with(&self.root));
        {
            let mut state = self.highlight.borrow_mut();
            if let Some(previous) = state
                .active
                .as_ref()
                .and_then(|path| state.rows.get(path))
                .and_then(glib::WeakRef::upgrade)
            {
                previous.remove_css_class("active-file");
            }
            state.active = path.map(Path::to_owned);
            if let Some(row) = path
                .and_then(|path| state.rows.get(path))
                .and_then(glib::WeakRef::upgrade)
            {
                row.add_css_class("active-file");
            }
        }

        let Some(path) = path else { return };
        let mut position = 0;
        while let Some(row) = self.model.row(position) {
            if let Some(entry) = entry_for(row.clone()) {
                if entry.path == path {
                    return;
                }
                if entry.directory && path.starts_with(&entry.path) {
                    row.set_expanded(true);
                }
            }
            position += 1;
        }
    }
}

fn install_move_target(
    widget: &impl IsA<gtk::Widget>,
    root: &Path,
    notify: &Notify,
    directory: impl Fn() -> Option<PathBuf> + 'static,
) {
    let target = gtk::DropTarget::new(glib::BoxedAnyObject::static_type(), gdk::DragAction::MOVE);
    let directory = Rc::new(directory);
    let accept_directory = directory.clone();
    target.connect_accept(move |_, drop| {
        drop.formats()
            .contains_type(glib::BoxedAnyObject::static_type())
            && accept_directory().is_some()
    });
    let root = root.to_owned();
    let notify = notify.clone();
    target.connect_drop(move |_, value, _, _| {
        let Ok(object) = value.get::<glib::BoxedAnyObject>() else {
            return false;
        };
        let Ok(entry) = object.try_borrow::<Entry>() else {
            return false;
        };
        let Some(directory) = directory() else {
            return false;
        };
        let source = &entry.path;
        if !source.starts_with(&root)
            || directory.starts_with(source)
            || source.parent() == Some(&directory)
        {
            return false;
        }
        let Some(name) = source.file_name() else {
            return false;
        };
        let destination = gio::File::for_path(directory.join(name));
        match gio::File::for_path(source).move_(
            &destination,
            gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE,
            gio::Cancellable::NONE,
            None,
        ) {
            Ok(()) => true,
            Err(error) => {
                notify(&format!("Could not move item: {error}"));
                false
            }
        }
    });
    widget.add_controller(target);
}

fn set_badge(row: &gtk::TreeExpander, status: Option<crate::git_status::Status>) {
    let badge = row
        .child()
        .and_then(|content| content.last_child())
        .and_downcast::<gtk::Label>()
        .expect("status label");
    if let Some(status) = status {
        badge.set_text(&status.badge());
        badge.set_tooltip_text(Some(&status.tooltip()));
    }
    badge.set_visible(status.is_some());
}

fn watch_status(project: &Path, widget: &gtk::Box, rows: &Rc<RefCell<Highlight>>) {
    let project = project.to_owned();
    let sidebar = widget.downgrade();
    let rows = Rc::downgrade(rows);
    glib::spawn_future_local(async move {
        let project_dir = project.clone();
        let Ok(Some((repo, prefix))) = gio::spawn_blocking(move || {
            let repo = crate::git_status::repository(&project_dir)?;
            let prefix = project_dir
                .canonicalize()
                .ok()?
                .strip_prefix(repo.canonicalize().ok()?)
                .ok()?
                .to_owned();
            Some((repo, prefix))
        })
        .await
        else {
            return;
        };
        loop {
            let Some(mapped) = sidebar.upgrade().map(|widget| widget.is_mapped()) else {
                break;
            };
            if mapped {
                let repo = repo.clone();
                let Ok(files) = gio::spawn_blocking(move || crate::git_status::read(&repo)).await
                else {
                    break;
                };
                let Some(rows) = rows.upgrade() else { break };
                let mut state = rows.borrow_mut();
                let statuses = files
                    .into_iter()
                    .filter_map(|(path, status)| {
                        Some((project.join(path.strip_prefix(&prefix).ok()?), status))
                    })
                    .collect::<HashMap<_, _>>();
                if statuses != state.statuses {
                    for (path, widget) in &state.rows {
                        if statuses.get(path) != state.statuses.get(path) {
                            if let Some(widget) = widget.upgrade() {
                                set_badge(&widget, statuses.get(path).copied());
                            }
                        }
                    }
                    state.statuses = statuses;
                }
            }
            glib::timeout_future(std::time::Duration::from_secs(2)).await;
        }
    });
}

fn entry_for(row: gtk::TreeListRow) -> Option<Entry> {
    Some(
        row.item()
            .and_downcast::<glib::BoxedAnyObject>()?
            .borrow::<Entry>()
            .clone(),
    )
}

pub fn set_file_icon(image: &gtk::Image, path: &Path) {
    if let Some(name) = icon_name(path) {
        image.set_icon_name(Some(name));
    } else {
        let (mime, _) = gio::content_type_guess(Some(path), None);
        image.set_from_gicon(&gio::content_type_get_symbolic_icon(&mime));
    }
}

#[derive(PartialEq)]
pub enum ImageKind {
    Svg,
    External,
}

pub fn image_kind(path: &Path) -> Option<ImageKind> {
    let file = gio::File::for_path(path);
    let info = file
        .query_info(
            "standard::content-type,standard::type",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok()?;
    if info.file_type() != gio::FileType::Regular {
        return None;
    }
    let mime = gio::content_type_get_mime_type(&info.content_type()?)?;
    match mime.as_str() {
        "image/svg+xml" => Some(ImageKind::Svg),
        mime if mime.starts_with("image/") => Some(ImageKind::External),
        _ => None,
    }
}

pub fn view_image(path: &Path, parent: &adw::ApplicationWindow, notify: &Notify) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let notify = notify.clone();
    launcher.launch(Some(parent), gio::Cancellable::NONE, move |result| {
        if let Err(error) = result {
            notify(&format!("Could not open image: {error}"));
        }
    });
}

fn icon_name(path: &Path) -> Option<&'static str> {
    Some(match path.file_name()?.to_string_lossy().as_ref() {
        "Cargo.toml" | "rust-toolchain" | "rust-toolchain.toml" => "text-rust-symbolic",
        "Cargo.lock"
        | "package-lock.json"
        | "npm-shrinkwrap.json"
        | "pnpm-lock.yaml"
        | "composer.lock"
        | "Gemfile.lock"
        | "Pipfile.lock"
        | "bun.lockb" => "lock-small-symbolic",
        "meson.build" | "CMakeLists.txt" | "Dockerfile" | "Containerfile" => {
            "text-makefile-symbolic"
        }
        ".env" | ".envrc" => "ui-attributes-symbolic",
        "Makefile" | "makefile" | "GNUmakefile" => "text-makefile-symbolic",
        "README" | "README.md" | "README.rst" => "text-x-readme-symbolic",
        "AUTHORS" => "text-x-authors-symbolic",
        "CHANGELOG" | "CHANGELOG.md" => "text-x-changelog-symbolic",
        "COPYING" | "LICENSE" => "text-x-copying-symbolic",
        _ => match path
            .extension()?
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_str()
        {
            "lock" => "lock-small-symbolic",
            "toml" | "ini" | "conf" | "cfg" | "yaml" | "yml" | "json" | "jsonc" => {
                "ui-attributes-symbolic"
            }
            "ino" => "text-arduino-symbolic",
            "hpp" | "hh" | "hxx" => "text-x-chdr-symbolic",
            "rs" => "text-rust-symbolic",
            "py" => "text-x-python-symbolic",
            "js" | "ts" | "jsx" | "tsx" | "mjs" => "text-x-javascript-symbolic",
            "css" => "text-css-symbolic",
            "html" | "htm" => "text-html-symbolic",
            "php" => "application-x-php-symbolic",
            "go" => "text-x-go-symbolic",
            "rb" => "text-x-ruby-symbolic",
            "c" => "text-x-csrc-symbolic",
            "h" => "text-x-chdr-symbolic",
            "cpp" | "cc" | "cxx" | "c++" => "text-x-c++src-symbolic",
            "sql" => "text-sql-symbolic",
            "xml" => "text-xml-symbolic",
            "vala" => "text-x-vala-symbolic",
            "md" | "markdown" => "text-markdown-symbolic",
            "swift" => "text-swift-symbolic",
            "sh" | "bash" | "zsh" | "fish" => "text-x-script-symbolic",
            "blp" => "text-x-blueprint-symbolic",
            _ => return None,
        },
    })
}

fn context_menu(
    anchor: &gtk::TreeExpander,
    parent: &adw::ApplicationWindow,
    entry: Entry,
    open: OpenFile,
    notify: Notify,
) {
    let menu = gio::Menu::new();
    let actions = gio::SimpleActionGroup::new();
    let add = |label: &str, id: &str, operation| {
        menu.append(Some(label), Some(&format!("file.{id}")));
        let action = gio::SimpleAction::new(id, None);
        let entry = entry.clone();
        let parent = parent.downgrade();
        let open = open.clone();
        let notify = notify.clone();
        action.connect_activate(move |_, _| {
            if let Some(parent) = parent.upgrade() {
                file_action(operation, &entry, &parent, &open, &notify);
            }
        });
        actions.add_action(&action);
    };
    if !entry.directory {
        add("Open", "open", FileAction::Open);
        if image_kind(&entry.path) == Some(ImageKind::Svg) {
            add("Open in Image Viewer", "view-image", FileAction::ViewImage);
        }
    }
    add("Show in Files", "reveal", FileAction::Reveal);
    add("Copy path", "copy", FileAction::Copy);
    add("Rename…", "rename", FileAction::Rename);
    add("Move to Trash", "trash", FileAction::Trash);
    add("Delete permanently…", "delete", FileAction::Delete);
    add("New file…", "new-file", FileAction::NewFile);
    add("New folder…", "new-folder", FileAction::NewFolder);
    let popup = gtk::PopoverMenu::from_model(Some(&menu));
    popup.add_css_class("file-context-menu");
    popup.insert_action_group("file", Some(&actions));
    let Some(sidebar) = std::iter::successors(anchor.parent(), |widget| widget.parent())
        .find(|widget| widget.has_css_class("helix-sidebar"))
    else {
        return;
    };
    let Some(row) = anchor.compute_bounds(&sidebar) else {
        return;
    };
    popup.set_parent(&sidebar);
    popup.set_position(gtk::PositionType::Right);
    popup.set_has_arrow(true);
    popup.set_pointing_to(Some(&gdk::Rectangle::new(
        sidebar.width() - 1,
        (row.y() + row.height() / 2.0).round() as i32,
        1,
        1,
    )));
    // GTK closes the menu before finishing action dispatch. Keep its action
    // group and widget hierarchy alive until the activation has returned.
    anchor.add_css_class("context-target");
    let anchor = anchor.downgrade();
    popup.connect_closed(move |popup| {
        if let Some(anchor) = anchor.upgrade() {
            anchor.remove_css_class("context-target");
        }
        let popup = popup.clone();
        glib::idle_add_local_once(move || popup.unparent());
    });
    popup.popup();
}

#[derive(Clone, Copy)]
enum FileAction {
    Open,
    ViewImage,
    Reveal,
    Copy,
    Rename,
    Trash,
    Delete,
    NewFile,
    NewFolder,
}

fn file_action(
    action: FileAction,
    entry: &Entry,
    parent: &adw::ApplicationWindow,
    open: &OpenFile,
    notify: &Notify,
) {
    let path = &entry.path;
    let directory = if entry.directory {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    match action {
        FileAction::Open => open(path),
        FileAction::ViewImage => view_image(path, parent, notify),
        FileAction::Copy => parent.clipboard().set_text(&path.to_string_lossy()),
        FileAction::Reveal => {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(directory)));
            let notify = notify.clone();
            launcher.launch(Some(parent), gio::Cancellable::NONE, move |result| {
                if let Err(error) = result {
                    notify(&format!("Could not open Files: {error}"));
                }
            });
        }
        FileAction::Trash => {
            if let Err(error) = gio::File::for_path(path).trash(gio::Cancellable::NONE) {
                notify(&format!("Could not move to Trash: {error}"));
            }
        }
        FileAction::Delete => {
            let dialog = adw::MessageDialog::new(
                Some(parent),
                Some("Delete permanently?"),
                Some(&entry.name),
            );
            dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
            dialog.set_close_response("cancel");
            dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let entry = entry.clone();
            let notify = notify.clone();
            dialog.connect_response(Some("delete"), move |_, _| {
                let result = if entry.directory {
                    fs::remove_dir(&entry.path)
                } else {
                    fs::remove_file(&entry.path)
                };
                if let Err(error) = result {
                    notify(&format!("Could not delete: {error}"));
                }
            });
            dialog.present();
        }
        FileAction::Rename | FileAction::NewFile | FileAction::NewFolder => {
            let heading = match action {
                FileAction::Rename => "Rename",
                FileAction::NewFile => "New file",
                _ => "New folder",
            };
            let dialog = adw::MessageDialog::new(Some(parent), Some(heading), None);
            let input = gtk::Entry::new();
            input.set_placeholder_text(Some("Name"));
            input.set_activates_default(true);
            if matches!(action, FileAction::Rename) {
                input.set_text(&entry.name);
            }
            dialog.set_extra_child(Some(&input));
            dialog.add_responses(&[("cancel", "Cancel"), ("apply", heading)]);
            dialog.set_close_response("cancel");
            dialog.set_default_response(Some("apply"));
            dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
            let path = path.clone();
            let directory = directory.to_path_buf();
            let notify = notify.clone();
            dialog.connect_response(Some("apply"), move |_, _| {
                let raw = input.text();
                let name = raw.trim();
                let mut components = Path::new(name).components();
                if !matches!(components.next(), Some(Component::Normal(part)) if part == name)
                    || components.next().is_some()
                {
                    notify("Enter a single file or folder name.");
                    return;
                }
                let result = match action {
                    FileAction::Rename => {
                        if path.file_name().is_some_and(|current| current == name) {
                            return;
                        }
                        gio::File::for_path(&path)
                            .set_display_name(name, gio::Cancellable::NONE)
                            .map(|_| ())
                            .map_err(std::io::Error::other)
                    }
                    FileAction::NewFolder => fs::create_dir(directory.join(name)),
                    _ => OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(directory.join(name))
                        .map(|_| ()),
                };
                if let Err(error) = result {
                    notify(&format!("{heading}: {error}"));
                }
            });
            dialog.present();
        }
    }
}
