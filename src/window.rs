use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};

use crate::{editor::Editor, files::Tree, terminal::Panel};

#[derive(Default, Deserialize, Serialize)]
struct Recent {
    #[serde(default)]
    recent_projects: Vec<PathBuf>,
}

impl Recent {
    fn path() -> PathBuf {
        glib::user_config_dir().join("hag/state.json")
    }

    fn load() -> Self {
        let mut recent: Self = fs::read(Self::path())
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    fs::read(glib::home_dir().join(".config/helix-ui/state.json"))
                } else {
                    Err(error)
                }
            })
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        recent.recent_projects.retain(|path| path.is_dir());
        recent
    }

    fn add(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut recent = Self::load();
        recent.recent_projects.retain(|current| current != path);
        recent.recent_projects.insert(0, path.to_owned());
        recent.recent_projects.truncate(10);
        let target = Self::path();
        fs::create_dir_all(target.parent().expect("state directory"))?;
        let bytes = serde_json::to_vec_pretty(&recent)?;
        gio::File::for_path(&target).replace_contents(
            &bytes,
            None,
            false,
            gio::FileCreateFlags::NONE,
            gio::Cancellable::NONE,
        )?;
        Ok(())
    }
}

struct Project {
    path: PathBuf,
    editor: Rc<Editor>,
    open_file: crate::files::OpenFile,
    _tree: Rc<Tree>,
    sidebar: gtk::ToggleButton,
    terminal: gtk::ToggleButton,
    panel: Panel,
}

type Current = Rc<RefCell<Option<Rc<Project>>>>;

pub fn build(app: &adw::Application, initial_project: Option<PathBuf>) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .icon_name("HAG")
        .title("HAG")
        .default_width(1400)
        .default_height(900)
        .build();
    let toast = adw::ToastOverlay::new();
    window.set_content(Some(&toast));
    let current: Current = Rc::default();

    if let Some(path) = initial_project {
        open_project(&window, &toast, &current, path);
    }
    if current.borrow().is_none() {
        toast.set_child(Some(&welcome(&window, &toast, &current)));
    }

    let commands: [(
        &str,
        &[&str],
        fn(&Project, &adw::ApplicationWindow, &adw::ToastOverlay),
    ); 5] = [
        ("toggle-sidebar", &["<Control>b"], |project, _, _| {
            project.sidebar.set_active(!project.sidebar.is_active());
        }),
        (
            "open-palette",
            &["<Control>p", "<Control>space"],
            |project, parent, _| {
                let open = project.open_file.clone();
                crate::palette::show(parent, &project.path, move |path| open(path));
            },
        ),
        ("toggle-terminal", &["<Control>t"], |project, _, _| {
            project.terminal.set_active(!project.terminal.is_active());
        }),
        (
            "new-terminal",
            &["<Control><Shift>t"],
            |project, _, toast| {
                if !project.panel.is_empty() || project.terminal.is_active() {
                    project.panel.add(&project.path, toast);
                }
                project.terminal.set_active(true);
            },
        ),
        (
            "open-git",
            &["<Control><Shift>g"],
            |project, parent, toast| {
                crate::terminal::git(parent, &project.path, toast);
            },
        ),
    ];
    for (name, accelerators, run) in commands {
        let action = gio::SimpleAction::new(name, None);
        let current = current.clone();
        let parent = window.downgrade();
        let toast = toast.downgrade();
        action.connect_activate(move |_, _| {
            let project = current.borrow().clone();
            if let (Some(project), Some(parent), Some(toast)) =
                (project, parent.upgrade(), toast.upgrade())
            {
                run(&project, &parent, &toast);
            }
        });
        window.add_action(&action);
        app.set_accels_for_action(&format!("win.{name}"), accelerators);
    }
    let action = gio::SimpleAction::new("open-project", None);
    let parent = window.downgrade();
    let overlay = toast.downgrade();
    let selected = current.clone();
    action.connect_activate(move |_, _| {
        if let (Some(window), Some(toast)) = (parent.upgrade(), overlay.upgrade()) {
            choose_project(&window, &toast, &selected);
        }
    });
    window.add_action(&action);
    app.set_accels_for_action("win.open-project", &["<Control>o"]);
    window.connect_is_active_notify(move |window| {
        if window.is_active() {
            if let Some(project) = current.borrow().as_ref() {
                project.editor.prune_missing();
            }
        }
    });
    window.present();
}

fn welcome(
    parent: &adw::ApplicationWindow,
    toast: &adw::ToastOverlay,
    current: &Current,
) -> adw::ToolbarView {
    let view = adw::ToolbarView::new();
    view.add_css_class("welcome-toolbar");
    view.add_top_bar(&adw::HeaderBar::new());
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.add_css_class("welcome-view");
    let bytes = glib::Bytes::from_static(include_bytes!("../icons/hicolor/scalable/apps/HAG.svg"));
    let icon = gtk::Image::from_gicon(&gio::BytesIcon::new(&bytes));
    icon.set_pixel_size(64);
    icon.add_css_class("dim-label");
    let title = gtk::Label::new(Some("HAG"));
    title.add_css_class("title-1");
    let subtitle = gtk::Label::new(Some("Open a folder to start editing with Helix."));
    subtitle.add_css_class("dim-label");
    let open = gtk::Button::with_label("Open Folder…");
    open.set_halign(gtk::Align::Center);
    open.set_action_name(Some("win.open-project"));
    open.add_css_class("suggested-action");
    open.add_css_class("pill");
    for child in [
        icon.upcast::<gtk::Widget>(),
        title.upcast(),
        subtitle.upcast(),
        open.upcast(),
    ] {
        content.append(&child);
    }

    let recent = Recent::load();
    if !recent.recent_projects.is_empty() {
        let label = gtk::Label::new(Some("Recent Projects"));
        label.set_xalign(0.0);
        label.add_css_class("heading");
        content.append(&label);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        for path in recent.recent_projects {
            let row = adw::ActionRow::new();
            row.set_title(&glib::markup_escape_text(
                &path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy(),
            ));
            row.set_subtitle(&glib::markup_escape_text(&path.to_string_lossy()));
            row.set_activatable(true);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let parent = parent.downgrade();
            let toast = toast.downgrade();
            let current = current.clone();
            row.connect_activated(move |_| {
                if let (Some(parent), Some(toast)) = (parent.upgrade(), toast.upgrade()) {
                    open_project(&parent, &toast, &current, path.clone());
                }
            });
            list.append(&row);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(400)
            .max_content_height(340)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        content.append(&scroll);
    }
    view.set_content(Some(&content));
    view
}

fn choose_project(window: &adw::ApplicationWindow, toast: &adw::ToastOverlay, current: &Current) {
    let dialog = gtk::FileDialog::builder()
        .title("Open project")
        .modal(true)
        .build();
    let parent = window.downgrade();
    let toast = toast.downgrade();
    let current = current.clone();
    dialog.select_folder(Some(window), gio::Cancellable::NONE, move |result| {
        if let (Ok(folder), Some(window), Some(toast)) = (result, parent.upgrade(), toast.upgrade())
        {
            if let Some(path) = folder.path() {
                if current.borrow().is_some() {
                    if let Some(app) = window.application().and_downcast::<adw::Application>() {
                        build(&app, Some(path));
                    }
                } else {
                    open_project(&window, &toast, &current, path);
                }
            } else {
                toast.add_toast(adw::Toast::new("Choose a local folder."));
            }
        }
    });
}

fn open_project(
    parent: &adw::ApplicationWindow,
    toast: &adw::ToastOverlay,
    current: &Current,
    path: PathBuf,
) {
    if !path.is_dir() || path.to_str().is_none() {
        toast.add_toast(adw::Toast::new("Open a local folder with a UTF-8 path."));
        return;
    }
    if let Err(error) = Recent::add(&path) {
        toast.add_toast(adw::Toast::new(&format!(
            "Could not save recent projects: {error}"
        )));
    }

    let tree_ref = Rc::new(RefCell::new(Weak::<Tree>::new()));
    let editor = Editor::new(&path, toast, {
        let tree_ref = tree_ref.clone();
        move |file| {
            if let Some(tree) = tree_ref.borrow().upgrade() {
                tree.highlight(file);
            }
        }
    });
    let weak = Rc::downgrade(&editor);
    let toast_ref = toast.downgrade();
    let notify: crate::files::Notify = Rc::new(move |message| {
        if let Some(toast) = toast_ref.upgrade() {
            toast.add_toast(adw::Toast::new(message));
        }
    });
    let window = parent.downgrade();
    let open_file: crate::files::OpenFile = {
        let notify = notify.clone();
        Rc::new(move |file| {
            if crate::files::image_kind(file) == Some(crate::files::ImageKind::External) {
                if let Some(window) = window.upgrade() {
                    crate::files::view_image(file, &window, &notify);
                }
            } else if let Some(editor) = weak.upgrade() {
                editor.open(file);
            }
        })
    };
    let tree = Rc::new(Tree::new(&path, parent, open_file.clone(), notify));
    *tree_ref.borrow_mut() = Rc::downgrade(&tree);

    let sidebar = toggle("sidebar-show-symbolic", "Toggle sidebar (Ctrl+B)", true);
    let terminal = toggle(
        "utilities-terminal-symbolic",
        "Toggle terminals (Ctrl+T)",
        false,
    );
    let panel = Panel::new(&terminal);
    let project = Rc::new(Project {
        path,
        editor,
        open_file,
        _tree: tree.clone(),
        sidebar,
        terminal,
        panel,
    });

    let header = adw::HeaderBar::new();
    header.add_css_class("content-header");
    header.set_decoration_layout(Some(":close"));
    header.pack_start(&project.sidebar);
    header.pack_start(&crate::icon_button(
        "document-open-symbolic",
        "Open folder in a new window (Ctrl+O)",
        "win.open-project",
    ));
    header.pack_start(&project.terminal);

    let terminal_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    terminal_container.add_css_class("editor-container");
    terminal_container.append(&project.editor.terminal);
    let vertical = gtk::Paned::new(gtk::Orientation::Vertical);
    vertical.set_start_child(Some(&terminal_container));
    vertical.set_end_child(Some(&project.panel.widget));
    vertical.set_resize_end_child(false);
    vertical.set_shrink_start_child(false);
    vertical.set_shrink_end_child(false);

    let top = gtk::Box::new(gtk::Orientation::Vertical, 0);
    top.add_css_class("editor-top-bar");
    top.append(&header);
    top.append(&project.editor.bar);
    let view = adw::ToolbarView::new();
    view.add_css_class("editor-content-pane");
    view.add_top_bar(&top);
    view.set_content(Some(&vertical));
    let split = gtk::Paned::new(gtk::Orientation::Horizontal);
    split.set_start_child(Some(&tree.widget));
    split.set_end_child(Some(&view));
    split.set_position(220);
    split.set_resize_start_child(false);
    split.set_shrink_start_child(false);

    let sidebar = tree.widget.downgrade();
    project.sidebar.connect_toggled(move |toggle| {
        if let Some(sidebar) = sidebar.upgrade() {
            sidebar.set_visible(toggle.is_active());
        }
    });
    let weak = Rc::downgrade(&project);
    let overlay = toast.downgrade();
    project.terminal.connect_toggled(move |toggle| {
        if let (Some(project), Some(overlay)) = (weak.upgrade(), overlay.upgrade()) {
            project.panel.widget.set_visible(toggle.is_active());
            if toggle.is_active() && project.panel.is_empty() {
                project.panel.add(&project.path, &overlay);
            } else if !toggle.is_active() {
                project.editor.terminal.grab_focus();
            }
        }
    });

    toast.set_child(Some(&split));
    current.replace(Some(project.clone()));
    project.editor.terminal.grab_focus();
}

fn toggle(icon: &str, tooltip: &str, active: bool) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .icon_name(icon)
        .active(active)
        .tooltip_text(tooltip)
        .build();
    button.add_css_class("flat");
    button
}
