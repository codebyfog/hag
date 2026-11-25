use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk::glib;
use vte::prelude::*;

#[derive(PartialEq)]
enum Process {
    Starting,
    Ready,
    Stopped,
}

struct State {
    process: Process,
    pending: VecDeque<String>,
    pages: BTreeMap<PathBuf, adw::TabPage>,
    active: Option<PathBuf>,
}

pub struct Editor {
    pub terminal: vte::Terminal,
    pub bar: adw::TabBar,
    tabs: adw::TabView,
    state: RefCell<State>,
    on_active: Box<dyn Fn(Option<&Path>)>,
    toast: adw::ToastOverlay,
}

impl Editor {
    pub fn new(
        root: &Path,
        toast: &adw::ToastOverlay,
        on_active: impl Fn(Option<&Path>) + 'static,
    ) -> Rc<Self> {
        let tabs = adw::TabView::new();
        let bar = adw::TabBar::new();
        bar.set_view(Some(&tabs));
        bar.set_autohide(false);
        bar.set_visible(false);

        let editor = Rc::new(Self {
            terminal: crate::terminal::widget(),
            bar,
            tabs,
            state: RefCell::new(State {
                process: Process::Starting,
                pending: VecDeque::new(),
                pages: BTreeMap::new(),
                active: None,
            }),
            on_active: Box::new(on_active),
            toast: toast.clone(),
        });
        editor.connect(root.to_path_buf());

        let program = glib::find_program_in_path("hx")
            .or_else(|| glib::find_program_in_path("helix"))
            .unwrap_or_else(|| PathBuf::from("hx"));
        let weak = Rc::downgrade(&editor);
        crate::terminal::spawn(
            &editor.terminal,
            root,
            &program.to_string_lossy(),
            move |result| {
                if let Some(editor) = weak.upgrade() {
                    match result {
                        Ok(_) => {
                            let weak = Rc::downgrade(&editor);
                            glib::timeout_add_local_once(Duration::from_millis(250), move || {
                                if let Some(editor) = weak.upgrade() {
                                    let mut state = editor.state.borrow_mut();
                                    if state.process == Process::Starting {
                                        state.process = Process::Ready;
                                        drop(state);
                                        editor.dispatch();
                                    }
                                }
                            });
                        }
                        Err(error) => editor.stopped(&format!("Could not start Helix: {error}")),
                    }
                }
            },
        );
        editor
    }

    fn connect(self: &Rc<Self>, root: PathBuf) {
        let weak = Rc::downgrade(self);
        self.tabs.connect_selected_page_notify(move |tabs| {
            let Some(editor) = weak.upgrade() else { return };
            let path = {
                let state = editor.state.borrow();
                tabs.selected_page().and_then(|page| {
                    state
                        .pages
                        .iter()
                        .find(|(path, candidate)| {
                            **candidate == page && state.active.as_ref() != Some(*path)
                        })
                        .map(|(path, _)| path.clone())
                })
            };
            if let Some(path) = path {
                editor.open(&path);
            }
        });

        let weak = Rc::downgrade(self);
        self.tabs.connect_close_page(move |_, page| {
            if let Some(editor) = weak.upgrade() {
                let path = editor
                    .state
                    .borrow()
                    .pages
                    .iter()
                    .find(|(_, candidate)| *candidate == page)
                    .map(|(path, _)| path.clone());
                if let Some(path) = path {
                    let mut state = editor.state.borrow_mut();
                    state.pages.remove(&path);
                    let was_active = state.active.as_deref() == Some(path.as_path());
                    if was_active {
                        state.active = None;
                    }
                    drop(state);
                    if was_active {
                        (editor.on_active)(None);
                    }
                    if path.exists() {
                        editor.command("buffer-close", &path);
                    }
                }
                editor
                    .bar
                    .set_visible(!editor.state.borrow().pages.is_empty());
            }
            glib::Propagation::Proceed
        });

        let weak = Rc::downgrade(self);
        self.terminal.connect_window_title_changed(move |terminal| {
            let Some(editor) = weak.upgrade() else { return };
            let Some(title) = terminal.window_title() else {
                return;
            };
            let Some((name, _)) = title.rsplit_once(" — ") else {
                return;
            };
            let path = if let Some(relative) = name.strip_prefix("~/") {
                glib::home_dir().join(relative)
            } else {
                root.join(name)
            };
            if !path.is_file() {
                return;
            }
            editor.state.borrow_mut().active = Some(path.clone());
            editor.select_tab(&path);
            (editor.on_active)(Some(&path));
        });

        let weak = Rc::downgrade(self);
        self.terminal.connect_child_exited(move |_, _| {
            if let Some(editor) = weak.upgrade() {
                editor.stopped("Helix exited. Reopen the project to start another session.");
            }
        });
    }

    fn stopped(&self, message: &str) {
        let mut state = self.state.borrow_mut();
        state.process = Process::Stopped;
        state.pending.clear();
        state.active = None;
        drop(state);
        (self.on_active)(None);
        self.toast.add_toast(adw::Toast::new(message));
    }

    fn select_tab(&self, path: &Path) {
        let existing = self.state.borrow().pages.get(path).cloned();
        let page = existing.unwrap_or_else(|| {
            let page = self
                .tabs
                .append(&gtk::Box::new(gtk::Orientation::Vertical, 0));
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            page.set_title(&name);
            page.set_tooltip(&glib::markup_escape_text(&path.to_string_lossy()));
            self.state
                .borrow_mut()
                .pages
                .insert(path.to_owned(), page.clone());
            self.bar.set_visible(true);
            page
        });
        self.tabs.set_selected_page(&page);
    }

    pub fn open(self: &Rc<Self>, path: &Path) {
        if !path.is_file() {
            self.toast
                .add_toast(adw::Toast::new("File is unavailable."));
            return;
        }
        if self.command("open", path) {
            self.state.borrow_mut().active = Some(path.to_owned());
            self.select_tab(path);
            (self.on_active)(Some(path));
        }
        self.terminal.grab_focus();
    }

    pub fn prune_missing(&self) {
        let removed: Vec<_> = self
            .state
            .borrow()
            .pages
            .iter()
            .filter(|(path, _)| !path.exists())
            .map(|(_, page)| page.clone())
            .collect();
        for page in removed {
            self.tabs.close_page(&page);
        }
    }

    fn command(self: &Rc<Self>, command: &str, path: &Path) -> bool {
        let Some(argument) = quote_path(path) else {
            self.toast
                .add_toast(adw::Toast::new("This path cannot be sent to Helix."));
            return false;
        };
        let mut state = self.state.borrow_mut();
        if state.process == Process::Stopped {
            self.toast
                .add_toast(adw::Toast::new("The Helix session has ended."));
            return false;
        }
        // CSI-u encodes Escape unambiguously even when followed immediately by a colon.
        // A bracketed paste inserts the complete command into the prompt in one event.
        state
            .pending
            .push_back(format!("\x1b[27u:\x1b[200~{command} {argument}\x1b[201~\r"));
        drop(state);
        self.dispatch();
        true
    }

    fn dispatch(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        if state.process == Process::Ready {
            for input in state.pending.drain(..) {
                self.terminal.feed_child(input.as_bytes());
            }
        }
    }
}

fn quote_path(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    if path.chars().any(char::is_control) {
        return None;
    }
    // Helix single quotes preserve literal whitespace and expansion markers.
    Some(format!("'{}'", path.replace('\'', "''")))
}
