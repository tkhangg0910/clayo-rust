//! The main window (MainWindow.xaml): sidebar | splitter | header + panes.
//!
//! Background work never depends on `ui`: panes run on their PTY threads, transcripts on the
//! sessions thread ([`crate::model`]), and pane events are handled in `App::logic`, which
//! eframe keeps calling while the window is hidden. Closing the window only hides it.

use crate::model::{self, lock, Job, Model, Shared};
use crate::pane::{AppEvent, Pane, Ticker, Waker};
use crate::theme::{self, *};
use crate::win;
use chrono::{Local, Utc};
use clayo_core::agent::claude::ClaudeAgent;
use clayo_core::agent::codex::CodexAgent;
use clayo_core::agent::AgentKind;
use clayo_core::launch::{self, plan, Agent, LaunchMode, LaunchPlan};
use clayo_core::pane_status::PaneStatus;
use clayo_core::paths::{claude_dir, data_dir, DataDir, OsEnv};
use clayo_core::security::ask_rules;
use clayo_core::settings::{self, ClayoSettings};
use clayo_core::sidebar::{self, Bucket, PaneRef, SortMode, UnboundPane, ViewOptions, Waiting};
use clayo_core::status::relay;
use clayo_core::system::login_startup::LoginStartup;
use clayo_term::input::{bracketed_path, paste_bytes};
use clayo_term::widget::{self, TermFont};
use clayo_term::GridSize;
use egui::{Popup, PopupCloseBehavior};
use egui::{
    Align2, Color32, CursorIcon, Id, Key, Margin, Pos2, Rect, RectAlign, Sense, SetOpenCommand, Stroke, StrokeKind,
    TextEdit, Ui, UiBuilder, Vec2, ViewportCommand,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Sidebar 310 (240..520) | 7 splitter | terminal (min 360).
const SIDEBAR_W: f32 = 310.0;
const SIDEBAR_MIN: f32 = 240.0;
const SIDEBAR_MAX: f32 = 520.0;
const SPLITTER_W: f32 = 7.0;
const TERMINAL_MIN: f32 = 360.0;
const HEADER_H: f32 = 42.0;
const ROW_H: f32 = 48.0;
const EMPTY_PANES: &str = "Pick a session on the left, or start a new one.";

/// How the app was asked to start (see `clayo_core::cli`).
pub struct Startup {
    pub background: bool,
    pub folder: Option<String>,
}

pub fn run(start: Startup) -> i32 {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("C L A Y O")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([760.0, 420.0])
        .with_visible(!start.background);
    let mut centered = true;
    #[cfg(windows)]
    if let Some((x, y, w, h)) = win::primary_work_area() {
        // Leave room for the caption (~32) and borders inside the work area.
        let (iw, ih) = (1280f32.min(w - 16.0), 820f32.min(h - 48.0));
        viewport = viewport.with_inner_size([iw, ih]).with_position([x + (w - iw) / 2.0, y + (h - ih - 32.0) / 2.0]);
        centered = false;
    }
    let opts = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered,
        // Key echo must not wait for the next vblank (PORTING.md §4.6, < 5 ms).
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() },
        clipboard_shortcuts: false,
        persist_window: false,
        ..Default::default()
    };
    crate::mark("run_native");
    match eframe::run_native("Clayo", opts, Box::new(move |cc| Ok(Box::new(ClayoApp::new(cc, start))))) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// Brings the main window forward from any thread (the island's click, a second instance):
/// shown if hidden, restored if minimized (a maximized window comes back maximized), focused.
#[derive(Clone)]
pub struct Revealer {
    flag: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl Revealer {
    #[allow(dead_code)] // Called by the island (phase 5) and the single instance (phase 6).
    pub fn reveal(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.ctx.request_repaint();
    }
}

/// The agent choice menu over New session is open for this request.
struct AskAgent {
    folder: String,
    insert: Option<String>,
}

struct Rename {
    id: String,
    text: String,
    started: bool,
}

/// One line of the sidebar list.
enum Item {
    Section(Bucket, usize),
    Folder(String, usize),
    Row(String, Bucket),
}

#[derive(Default)]
struct ListCache {
    key: Option<(u64, ViewOptions, i64)>,
    items: Vec<Item>,
    signature: String,
}

/// Things the UI asked for this frame, run after drawing (dialogs block).
enum Action {
    Select(String),
    Open(String),
    StartRename(String),
    CommitRename(String, String),
    ResetName(String),
    NewIn(String),
    Browse,
    StartWith(AgentKind),
    OpenFile,
    Attach,
    Branch,
    CloseActive,
    OpenSettings,
    CloseSettings,
    SetLogin(bool),
    Quit,
}

pub struct ClayoApp {
    settings: ClayoSettings,
    data: DataDir,
    model: Shared,
    jobs: Sender<Job>,
    panes: Vec<Pane>,
    active: Option<u64>,
    /// Open panes, the one used last first (the screenshot picker's order).
    used: Vec<u64>,
    next_id: u64,
    waker: Waker,
    rx: Receiver<AppEvent>,
    ticker: Ticker,
    font: TermFont,
    /// Where New session starts and the sidebar header points.
    folder: String,
    view: ViewOptions,
    sections: HashMap<Bucket, bool>,
    selected: Option<String>,
    list_focus: bool,
    rename: Option<Rename>,
    cache: ListCache,
    scroll_top: bool,
    sidebar_w: f32,
    settings_open: bool,
    focus_terminal: bool,
    ask_agent: Option<AskAgent>,
    open_agent_menu: bool,
    /// Start at login (on, can change), read fresh when the gear menu opens.
    login: (bool, bool),
    quitting: bool,
    shown: bool,
    reveal: Arc<AtomicBool>,
    revealer: Revealer,
    actions: Vec<Action>,
    first_frame_done: bool,
}

impl ClayoApp {
    fn new(cc: &eframe::CreationContext<'_>, start: Startup) -> Self {
        crate::mark("app new (window + GL ready)");
        cc.egui_ctx.set_fonts(crate::fonts::definitions());
        crate::mark("fonts");
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.send_viewport_cmd(ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        #[cfg(windows)]
        if let Ok(h) = raw_window_handle::HasWindowHandle::window_handle(cc) {
            if let raw_window_handle::RawWindowHandle::Win32(w) = h.as_raw() {
                win::dark_caption(w.hwnd.get());
            }
        }
        let (tx, rx) = channel();
        let env = OsEnv;
        let data = data_dir(&env);
        let settings = ClayoSettings::load(&settings::default_path(&data));
        let model: Shared =
            Arc::new(std::sync::Mutex::new(Model::new(&data, settings.use_claude, settings.use_codex)));
        let ctx = cc.egui_ctx.clone();
        let jobs = model::start_worker(model.clone(), move || ctx.request_repaint());
        let reveal = Arc::new(AtomicBool::new(false));
        let revealer = Revealer { flag: reveal.clone(), ctx: cc.egui_ctx.clone() };
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let mut app = Self {
            settings,
            data,
            model,
            jobs,
            panes: Vec::new(),
            active: None,
            used: Vec::new(),
            next_id: 1,
            waker: Waker { ctx: cc.egui_ctx.clone(), tx },
            rx,
            ticker: Ticker::start(),
            font: TermFont::default(),
            folder: start.folder.clone().unwrap_or(home),
            view: ViewOptions::default(),
            sections: HashMap::from([(Bucket::Open, true), (Bucket::Today, true), (Bucket::Earlier, false)]),
            selected: None,
            list_focus: false,
            rename: None,
            cache: ListCache::default(),
            scroll_top: false,
            sidebar_w: SIDEBAR_W,
            settings_open: false,
            focus_terminal: false,
            ask_agent: None,
            open_agent_menu: false,
            login: (false, false),
            quitting: false,
            shown: !start.background,
            reveal,
            revealer,
            actions: Vec::new(),
            first_frame_done: false,
        };
        // Opening Clayo in a folder lands you in a live session there.
        if let Some(folder) = start.folder {
            app.start_new(&folder, None);
            crate::mark("pane started");
        }
        app
    }

    /// For the island and the single instance (later phases).
    #[allow(dead_code)]
    pub fn revealer(&self) -> Revealer {
        self.revealer.clone()
    }

    // ------------------------------------------------------------------ launching

    fn agent(&self, kind: AgentKind) -> Box<dyn Agent> {
        let env = OsEnv;
        match kind {
            AgentKind::Codex => Box::new(CodexAgent::for_this_user(&env, self.settings.codex_path.as_deref())),
            AgentKind::Claude => {
                let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let relay = relay::write_settings(
                    &self.data,
                    &claude_dir(&env),
                    &exe,
                    &ask_rules::claude_ask(&self.settings),
                );
                Box::new(ClaudeAgent::for_this_user(&env, self.settings.claude_path.as_deref(), relay.as_deref()))
            }
        }
    }

    fn plan(&self, kind: AgentKind, mode: LaunchMode, folder: &str, id: Option<&str>) -> Option<LaunchPlan> {
        let mut p = plan(self.agent(kind).as_ref(), mode, folder, id, &OsEnv).ok()?;
        // Debug builds: run something else in place of the agent (rendering checks, benches).
        if cfg!(debug_assertions) {
            if let Ok(cmd) = std::env::var("CLAYO_DEV_AGENT_CMD") {
                p.agent_command = cmd;
            }
        }
        Some(p)
    }

    /// A new session with the agent settings pick, or the one chosen from the menu over New
    /// session when set to ask (closing that menu starts nothing).
    fn start_new(&mut self, folder: &str, insert: Option<String>) {
        match self.settings.new_session_kind() {
            Some(kind) => self.start_new_with(kind, folder, insert),
            None => {
                self.ask_agent = Some(AskAgent { folder: folder.to_string(), insert });
                self.open_agent_menu = true;
            }
        }
    }

    fn start_new_with(&mut self, kind: AgentKind, folder: &str, insert: Option<String>) {
        let Some(p) = self.plan(kind, LaunchMode::New, folder, None) else { return };
        let id = self.start_pane(&p, sidebar::new_title(folder), None);
        if let (Some(text), Some(pane)) = (insert, self.pane(id)) {
            pane.shared.insert_on_first_done(text);
        }
    }

    /// Points the window at `folder` and starts a session there (SpawnIn).
    fn spawn_in(&mut self, folder: &str, insert: Option<String>) {
        if !std::path::Path::new(folder).is_dir() {
            return;
        }
        self.folder = folder.to_string();
        self.start_new(folder, insert);
    }

    /// The row exists at once (status Starting), under Open, before any transcript.
    fn start_pane(&mut self, p: &LaunchPlan, title: String, fork_of: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let pane = Pane::start(
            id,
            p,
            title.clone(),
            GridSize { cols: 120, rows: 40 },
            None,
            self.waker.clone(),
            self.ticker.clone(),
        );
        {
            let mut m = lock(&self.model);
            let key = p.expected_session_id.clone().unwrap_or_else(clayo_core::text::new_guid);
            let custom = m.names.get(&key).map(String::from);
            let pref = PaneRef { id, agent: p.agent, status: pane.status, working_dir: p.working_directory.clone() };
            m.sidebar.open_pane(&key, pref, &title, custom);
            if p.expected_session_id.is_none() {
                m.waiting.push(Waiting {
                    placeholder: key.clone(),
                    pane: UnboundPane { folder: p.working_directory.clone(), parent_id: fork_of, since: Utc::now() },
                });
            }
            m.panes.push(pane.shared.clone());
            m.generation += 1;
            self.selected = Some(key);
        }
        self.panes.push(pane);
        self.activate(id);
        id
    }

    fn pane(&self, id: u64) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id() == id)
    }

    fn active_pane(&self) -> Option<&Pane> {
        self.active.and_then(|id| self.pane(id))
    }

    /// Switches to a pane; picking or starting a session also leaves Settings.
    fn activate(&mut self, id: u64) {
        self.settings_open = false;
        self.active = Some(id);
        self.used.retain(|u| *u != id);
        self.used.insert(0, id);
        self.focus_terminal = true;
        self.list_focus = false;
    }

    /// Ends the session: the process goes and its row drops out of Open. A Codex pane closed
    /// before its transcript appeared has no session to list. The last pane left takes over.
    fn close_pane(&mut self, id: u64) {
        let Some(i) = self.panes.iter().position(|p| p.id() == id) else { return };
        let pane = self.panes.remove(i);
        pane.shared.kill();
        {
            let mut m = lock(&self.model);
            let placeholder = m.sidebar.by_pane(id).map(|r| r.session_id.clone());
            let before = m.waiting.len();
            m.waiting.retain(|w| Some(&w.placeholder) != placeholder.as_ref());
            let unbound = m.waiting.len() != before;
            m.sidebar.close_pane(id, unbound);
            m.panes.retain(|p| p.id != id);
            m.generation += 1;
        }
        // Island (phase 5): SessionNotice(pane, "", null) and SessionWorking(pane, false) go here.
        self.used.retain(|u| *u != id);
        self.active = self.panes.last().map(Pane::id);
        if let Some(a) = self.active {
            self.activate(a);
        }
    }

    /// Resume a row's session in its folder (or the current one if that is gone), or switch to
    /// its pane if it is open.
    fn open_row(&mut self, id: &str) {
        let (pane, folder, agent, name) = {
            let m = lock(&self.model);
            let Some(r) = m.sidebar.get(id) else { return };
            (r.pane.as_ref().map(|p| p.id), r.folder.clone(), r.agent(), r.name().to_string())
        };
        if let Some(p) = pane {
            self.activate(p);
            return;
        }
        let cwd = launch::resume_folder(&folder, &self.folder);
        if let Some(p) = self.plan(agent, LaunchMode::Resume, &cwd, Some(id)) {
            self.start_pane(&p, name, None);
        }
    }

    fn branch(&mut self) {
        let Some(pane) = self.active_pane() else { return };
        let Some(parent) = pane.shared.session_id() else { return };
        let (agent, cwd) = (pane.agent, pane.cwd.to_string_lossy().into_owned());
        let Some(p) = self.plan(agent, LaunchMode::Fork, &cwd, Some(&parent)) else { return };
        // Record the link now: the fork rewrites sessionId on every line it copies, so nothing
        // on disk will say the two are related. A Codex fork names its parent itself.
        if let Some(child) = &p.expected_session_id {
            lock(&self.model).parents.set(child, &parent);
        }
        self.start_pane(&p, sidebar::branch_title(&parent), Some(parent));
    }

    /// Ends Clayo: every session closes, the watchers stop, the app exits.
    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        let _ = self.jobs.send(Job::Stop);
        for p in &self.panes {
            p.shared.kill();
        }
        self.panes.clear();
        lock(&self.model).panes.clear();
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn paste(&self, pane: &Pane) {
        let mode = pane.shared.term.mode();
        #[cfg(windows)]
        match win::read_clipboard() {
            win::Clip::Text(t) => pane.shared.type_keys(&paste_bytes(&t, mode)),
            win::Clip::Files(files) => {
                // Images go as bracketed paths (the agent attaches them as [Image #n]); other
                // files are typed, quoted when they hold a space (TP:342-368).
                let mut out = Vec::new();
                for f in files {
                    if clayo_core::images::is_image(std::path::Path::new(&f)) {
                        out.extend(bracketed_path(&f));
                        out.push(b' ');
                    } else {
                        out.extend(clayo_core::text::insert_paths(&[f]).into_bytes());
                    }
                }
                pane.shared.type_keys(&out);
            }
            win::Clip::Nothing => {}
        }
    }

    // ------------------------------------------------------------------ actions

    fn run_actions(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        for a in std::mem::take(&mut self.actions) {
            match a {
                Action::Select(id) => {
                    if self.rename.as_ref().is_some_and(|r| r.id != id) {
                        self.rename = None; // leaving a row cancels its rename
                    }
                    let pane = lock(&self.model).sidebar.get(&id).and_then(|r| r.pane.as_ref()).map(|p| p.id);
                    self.selected = Some(id);
                    if let Some(p) = pane {
                        self.activate(p);
                    }
                }
                Action::Open(id) => self.open_row(&id),
                Action::StartRename(id) => {
                    let text = lock(&self.model).sidebar.get(&id).map(|r| r.edit_text());
                    if let Some(text) = text {
                        self.rename = Some(Rename { id, text, started: false });
                    }
                }
                Action::CommitRename(id, text) => self.commit_rename(&id, &text),
                Action::ResetName(id) => {
                    let mut m = lock(&self.model);
                    m.names.set(&id, None);
                    if let Some(r) = m.sidebar.get_mut(&id) {
                        r.custom_name = None;
                    }
                    m.generation += 1;
                }
                Action::NewIn(folder) => self.spawn_in(&folder, None),
                Action::Browse => {
                    let mut d = rfd::FileDialog::new().set_title("Start a session in").set_parent(frame);
                    if std::path::Path::new(&self.folder).is_dir() {
                        d = d.set_directory(&self.folder);
                    }
                    if let Some(f) = d.pick_folder() {
                        self.spawn_in(&f.to_string_lossy(), None);
                    }
                }
                Action::StartWith(kind) => {
                    if let Some(ask) = self.ask_agent.take() {
                        self.start_new_with(kind, &ask.folder, ask.insert);
                    }
                }
                Action::OpenFile => {
                    let mut d = rfd::FileDialog::new().set_title("Open a file in a new session").set_parent(frame);
                    if std::path::Path::new(&self.folder).is_dir() {
                        d = d.set_directory(&self.folder);
                    }
                    if let Some(file) = d.pick_file() {
                        let text = clayo_core::text::insert_paths(&[file.to_string_lossy()]);
                        if let Some(dir) = file.parent() {
                            self.spawn_in(&dir.to_string_lossy(), Some(text));
                        }
                    }
                }
                Action::Attach => {
                    let Some(pane) = self.active_pane() else { continue };
                    let mut d = rfd::FileDialog::new().set_title("Attach files").set_parent(frame);
                    if pane.cwd.is_dir() {
                        d = d.set_directory(&pane.cwd);
                    }
                    if let Some(files) = d.pick_files() {
                        let files: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
                        pane.shared.insert_paths(&files);
                    }
                    self.focus_terminal = true;
                }
                Action::Branch => self.branch(),
                Action::CloseActive => {
                    if let Some(id) = self.active {
                        self.close_pane(id);
                    }
                }
                Action::OpenSettings => {
                    Popup::close_all(ctx);
                    self.settings_open = true;
                }
                Action::CloseSettings => {
                    self.settings_open = false;
                    self.focus_terminal = true;
                }
                Action::SetLogin(on) => {
                    #[cfg(windows)]
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = LoginStartup::for_this_user(&self.data).set(on, &exe);
                    }
                    #[cfg(not(windows))]
                    let _ = on;
                }
                Action::Quit => self.quit(ctx),
            }
        }
    }

    // ------------------------------------------------------------------ drawing

    fn rebuild_list(&mut self) {
        let now = Utc::now();
        let mut m = lock(&self.model);
        let key = (m.generation, self.view.clone(), now.timestamp() / 60);
        // Never under an open rename box: it would move or vanish mid-word.
        if self.cache.key.as_ref() == Some(&key) || self.rename.is_some() {
            return;
        }
        let m = &mut *m;
        let parents = &m.parents;
        m.sidebar.link_threads(|id| parents.get(id).map(String::from), self.view.sort != SortMode::Oldest, now);
        let rows = m.sidebar.view(&self.view, now, &Local);
        let signature = sidebar::layout_signature(&rows, self.view.group_by_folder, now, &Local);
        let mut items = Vec::new();
        let mut i = 0;
        while i < rows.len() {
            let b = rows[i].bucket(now, &Local);
            let n = rows[i..].iter().take_while(|r| r.bucket(now, &Local) == b).count();
            items.push(Item::Section(b, n));
            let mut j = i;
            while j < i + n {
                if self.view.group_by_folder {
                    let proj = rows[j].project();
                    let k = proj.to_lowercase();
                    let c = rows[j..i + n].iter().take_while(|r| r.project().to_lowercase() == k).count();
                    items.push(Item::Folder(proj, c));
                    for r in &rows[j..j + c] {
                        items.push(Item::Row(r.session_id.clone(), b));
                    }
                    j += c;
                } else {
                    items.push(Item::Row(rows[j].session_id.clone(), b));
                    j += 1;
                }
            }
            i += n;
        }
        if signature != self.cache.signature {
            self.scroll_top = true;
        }
        self.cache = ListCache { key: Some(key), items, signature };
    }

    fn draw_sidebar(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, SURFACE_BG);
        let footer_h = 12.0 + 30.0 + 14.0;
        let body = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom() - footer_h - 1.0));
        let footer = Rect::from_min_max(Pos2::new(rect.left(), body.bottom()), rect.max);
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| self.draw_sidebar_body(ui));
        ui.scope_builder(UiBuilder::new().max_rect(footer), |ui| self.draw_footer(ui));
    }

    fn draw_sidebar_body(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let inner = rect.width() - 28.0;
        // Header: current folder leaf over the full path.
        let x = rect.left() + 14.0;
        let mut y = rect.top() + 14.0;
        let leaf = clayo_core::paths::leaf(&self.folder);
        let g = elided(ui, &leaf, semibold(14.0), TEXT_PRIMARY, inner);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x, y), g, TEXT_PRIMARY);
        y += h + 2.0;
        let g = elided(ui, &self.folder, mono(10.5), TEXT_FAINT, inner);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x, y), g, TEXT_FAINT);
        y += h + 8.0;

        // Search: the hint goes as soon as the caret lands, and comes back on blur if empty.
        let search_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(inner, 29.0));
        let out = ui.scope_builder(UiBuilder::new().max_rect(search_rect), |ui| {
            TextEdit::singleline(&mut self.view.filter)
                .id(Id::NULL.with("search"))
                .font(ui_font(12.0))
                .text_color(TEXT_PRIMARY)
                .margin(Margin::symmetric(8, 6))
                .desired_width(inner)
                .frame(
                    egui::Frame::new()
                        .fill(TERMINAL_BG)
                        .stroke(Stroke::new(1.0, HAIRLINE))
                        .corner_radius(4)
                        .inner_margin(Margin::symmetric(8, 6)),
                )
                .show(ui)
        });
        let sresp = out.inner.response.response;
        if self.view.filter.is_empty() && !sresp.has_focus() {
            ui.painter().text(
                Pos2::new(sresp.rect.left() + 9.0, sresp.rect.center().y),
                Align2::LEFT_CENTER,
                "Search sessions",
                ui_font(12.0),
                TEXT_FAINT,
            );
        }
        if sresp.has_focus() {
            self.list_focus = false;
        }
        y = sresp.rect.bottom() + 6.0;

        // Group / sort, the Explorer idiom.
        let combos = Rect::from_min_size(Pos2::new(rect.left() + 10.0, y), Vec2::new(rect.width() - 20.0, 24.0));
        ui.scope_builder(UiBuilder::new().max_rect(combos).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            let group = if self.view.group_by_folder { "Group: Folder" } else { "Group: None" };
            let r = tiny_combo(ui, group);
            tiny_popup(&r, |ui| {
                for (label, v) in [("Group: Folder", true), ("Group: None", false)] {
                    if tiny_item(ui, label, self.view.group_by_folder == v).clicked() {
                        self.view.group_by_folder = v;
                        ui.close();
                    }
                }
            });
            ui.add_space(4.0);
            let sort = match self.view.sort {
                SortMode::Newest => "Newest",
                SortMode::Oldest => "Oldest",
                SortMode::Name => "Name",
            };
            let r = tiny_combo(ui, sort);
            tiny_popup(&r, |ui| {
                for (label, v) in [("Newest", SortMode::Newest), ("Oldest", SortMode::Oldest), ("Name", SortMode::Name)] {
                    if tiny_item(ui, label, self.view.sort == v).clicked() {
                        self.view.sort = v;
                        ui.close();
                    }
                }
            });
        });
        y = combos.bottom() + 4.0;

        let list = Rect::from_min_max(Pos2::new(rect.left(), y), rect.max);
        self.rebuild_list();
        ui.scope_builder(UiBuilder::new().max_rect(list), |ui| self.draw_list(ui));
    }

    fn draw_list(&mut self, ui: &mut Ui) {
        let now = Utc::now();
        let mut sa = egui::ScrollArea::vertical().id_salt("sessions").auto_shrink(false);
        if std::mem::take(&mut self.scroll_top) {
            sa = sa.vertical_scroll_offset(0.0);
        }
        let model = self.model.clone();
        let items = std::mem::take(&mut self.cache.items);
        sa.show(ui, |ui| {
            let m = lock(&model);
            for item in &items {
                match item {
                    Item::Section(b, n) => {
                        let open = self.sections.get(b).copied().unwrap_or(b.expanded_by_default());
                        if section_header(ui, b.label(), *n, open).clicked() {
                            self.sections.insert(*b, !open);
                        }
                    }
                    Item::Folder(name, n) => {
                        if self.section_open(item) {
                            folder_header(ui, name, *n);
                        }
                    }
                    Item::Row(id, _) => {
                        if !self.section_open(item) {
                            continue;
                        }
                        if let Some(row) = m.sidebar.get(id) {
                            self.draw_row(ui, row, now);
                        }
                    }
                }
            }
        });
        self.cache.items = items;
    }

    fn section_open(&self, item: &Item) -> bool {
        match item {
            Item::Row(_, b) => self.sections.get(b).copied().unwrap_or(b.expanded_by_default()),
            // A folder header sits in the section of the row after it.
            Item::Folder(..) => true,
            Item::Section(..) => true,
        }
    }

    fn draw_row(&mut self, ui: &mut Ui, row: &sidebar::SessionRow, now: chrono::DateTime<Utc>) {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
        let id = &row.session_id;
        let selected = self.selected.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(id));
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            if selected {
                p.rect_filled(rect, 0, ROW_SELECTED);
                p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())), 0, WORKING);
            } else if resp.hovered() {
                p.rect_filled(rect, 0, ROW_HOVER);
            }
            let x0 = rect.left() + 2.0 + 34.0 + row.depth as f32 * 15.0;
            let top = rect.top() + 7.0;
            dot(p, Pos2::new(x0 + 3.5, top + 7.5), status_color(row.status_key()));
            let mut x = x0 + 14.0;
            let right = rect.right() - 10.0;
            if row.is_branch() {
                let g = p.layout_no_wrap("↳".into(), ui_font(11.0), TEXT_FAINT);
                let gw = g.size().x;
                p.galley(Pos2::new(x, top + 1.0), g, TEXT_FAINT);
                x += gw + 5.0;
            }
            let renaming = self.rename.as_ref().is_some_and(|r| r.id.eq_ignore_ascii_case(id));
            if !renaming {
                let mut name_right = right;
                if let Some(tag) = row.agent_tag() {
                    let c = parse_hex(row.agent_color()).gamma_multiply(0.85);
                    let g = p.layout_no_wrap(tag.into(), semibold_mono(9.5), c);
                    name_right -= g.size().x + 6.0;
                    p.galley(Pos2::new(right - g.size().x, top + 3.0), g, c);
                }
                let g = elided(ui, row.name(), ui_font(12.5), TEXT_PRIMARY, name_right - x);
                p.galley(Pos2::new(x, top), g, TEXT_PRIMARY);
            }
            let sub = row.subtitle(now, &Local);
            let g = elided(ui, &sub, ui_font(10.5), TEXT_FAINT, right - (x0 + 14.0));
            p.galley(Pos2::new(x0 + 14.0, top + 20.0), g, TEXT_FAINT);
            if renaming {
                self.draw_rename(ui, Rect::from_min_max(Pos2::new(x, top - 3.0), Pos2::new(right, top + 18.0)));
            }
        }
        if resp.clicked() || resp.secondary_clicked() {
            self.actions.push(Action::Select(id.clone()));
            self.list_focus = true;
            resp.request_focus();
        }
        if resp.double_clicked() {
            self.actions.push(Action::Open(id.clone()));
        }
        let resp = if resp.hovered() && self.rename.is_none() { resp.on_hover_text(row.tooltip()) } else { resp };
        resp.context_menu(|ui| {
            if menu_item(ui, "Open", None, None, true).clicked() {
                self.actions.push(Action::Open(id.clone()));
                ui.close();
            }
            if menu_item(ui, "Rename", Some("F2"), None, true).clicked() {
                self.actions.push(Action::StartRename(id.clone()));
                ui.close();
            }
            menu_separator(ui);
            if menu_item(ui, "Reset name", None, None, true).clicked() {
                self.actions.push(Action::ResetName(id.clone()));
                ui.close();
            }
        });
    }

    /// The in-row rename box: focused with everything selected; Enter or leaving it commits,
    /// Esc cancels.
    fn draw_rename(&mut self, ui: &mut Ui, rect: Rect) {
        let Some(r) = self.rename.as_mut() else { return };
        let edit_id = Id::NULL.with("rename-box");
        let out = ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            TextEdit::singleline(&mut r.text)
                .id(edit_id)
                .font(ui_font(12.5))
                .text_color(TEXT_PRIMARY)
                .desired_width(rect.width())
                .frame(
                    egui::Frame::new()
                        .fill(TERMINAL_BG)
                        .stroke(Stroke::new(1.0, WORKING))
                        .corner_radius(3)
                        .inner_margin(Margin::symmetric(4, 1)),
                )
                .show(ui)
        });
        let mut out = out.inner;
        let resp = out.response.response.clone();
        if !r.started {
            r.started = true;
            resp.request_focus();
            out.state.cursor.set_char_range(Some(egui::text::CCursorRange::select_all_str(&r.text)));
            out.state.store(ui.ctx(), edit_id);
            return;
        }
        if resp.lost_focus() {
            let cancel = ui.input(|i| i.key_pressed(Key::Escape));
            let r = self.rename.take().expect("checked above");
            if !cancel {
                self.actions.push(Action::CommitRename(r.id, r.text));
            }
        }
    }

    fn commit_rename(&mut self, id: &str, text: &str) {
        let mut m = lock(&self.model);
        let Some(row) = m.sidebar.get(id) else { return };
        let custom = row.rename_to(text);
        m.names.set(id, custom.as_deref());
        if let Some(r) = m.sidebar.get_mut(id) {
            r.custom_name = custom;
        }
        m.generation += 1;
    }

    fn draw_footer(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, FOOTER_BAND);
        ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, HAIRLINE));
        // Slot: the account's limits (LimitsView) and the Update button go above these (phases 4, 6).
        let row = Rect::from_min_max(
            Pos2::new(rect.left() + 14.0, rect.top() + 1.0 + 12.0),
            Pos2::new(rect.right() - 14.0, rect.top() + 1.0 + 12.0 + 30.0),
        );
        ui.scope_builder(UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            let new = ghost_button(ui, Some('\u{E710}'), "New session", true, None)
                .on_hover_text("Pick the folder to start in. Recent folders first.");
            self.folder_popup(ui, &new);
            self.agent_menu(&new);
            ui.add_space(8.0);
            let open = ghost_button(ui, Some('\u{E8B7}'), "Open", false, None).on_hover_text(
                "Pick a file: a new session starts in its folder with the file ready at the prompt.",
            );
            if open.clicked() {
                self.actions.push(Action::OpenFile);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let gear = ghost_button(ui, Some('\u{E713}'), "", false, Some(30.0)).on_hover_text("Settings");
                if gear.clicked() {
                    let s = LoginStartup::for_this_user(&self.data);
                    #[cfg(windows)]
                    let on = s.is_on();
                    #[cfg(not(windows))]
                    let on = false;
                    self.login = (on, s.can_write());
                }
                Popup::menu(&gear)
                    .align(RectAlign::TOP_END)
                    .gap(6.0)
                    .frame(popup_frame(8, MENU_RIM))
                    .show(|ui| {
                        ui.set_min_width(176.0);
                        if menu_item(ui, "Settings…", Some("Ctrl+,"), None, true).clicked() {
                            self.actions.push(Action::OpenSettings);
                        }
                        let (on, can) = self.login;
                        if menu_item(ui, "Start at login", None, Some(on), can).clicked() {
                            self.actions.push(Action::SetLogin(!on));
                        }
                        menu_separator(ui);
                        if menu_item(ui, "Quit Clayo", None, None, true).clicked() {
                            self.actions.push(Action::Quit);
                        }
                    });
            });
        });
    }

    /// New session's folder picker, opening upward: recent folders, then Browse.
    fn folder_popup(&mut self, ui: &mut Ui, new: &egui::Response) {
        let choices = if new.clicked() || Popup::is_id_open(ui.ctx(), Popup::default_response_id(new)) {
            let exists = |f: &str| std::path::Path::new(f).is_dir();
            lock(&self.model).sidebar.recent_folders(&self.folder, 8, Utc::now(), exists)
        } else {
            Vec::new()
        };
        Popup::from_toggle_button_response(new)
            .align(RectAlign::TOP_START)
            .gap(6.0)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .frame(popup_frame(5, HAIRLINE))
            .show(|ui| {
                ui.set_min_width(272.0);
                ui.set_max_width(420.0);
                let w = ui.available_width().clamp(272.0, 420.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::hover());
                ui.painter().text(
                    Pos2::new(r.left() + 7.0, r.top() + 4.0),
                    Align2::LEFT_TOP,
                    "Start a session in",
                    ui_font(10.5),
                    TEXT_FAINT,
                );
                egui::ScrollArea::vertical().max_height(264.0).id_salt("recent").show(ui, |ui| {
                    for c in &choices {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 3, POPUP_HOVER);
                        }
                        let g = elided(ui, &c.name, ui_font(12.0), TEXT_PRIMARY, w - 14.0);
                        ui.painter().galley(Pos2::new(r.left() + 7.0, r.top() + 5.0), g, TEXT_PRIMARY);
                        let g = elided(ui, &c.full_path, mono(10.0), TEXT_FAINT, w - 14.0);
                        ui.painter().galley(Pos2::new(r.left() + 7.0, r.top() + 22.0), g, TEXT_FAINT);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            self.actions.push(Action::NewIn(c.full_path.clone()));
                            ui.close();
                        }
                    }
                });
                ui.add_space(5.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
                ui.painter().hline(r.x_range().shrink(4.0), r.center().y, Stroke::new(1.0, HAIRLINE));
                ui.add_space(7.0);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let b = quiet_button_wide(ui, "Browse for a folder…", w - 8.0);
                    if b.clicked() {
                        self.actions.push(Action::Browse);
                        ui.close();
                    }
                });
                ui.add_space(2.0);
            });
    }

    /// Set to ask each time: "Claude Code" / "Codex" over New session; closing starts nothing.
    fn agent_menu(&mut self, new: &egui::Response) {
        if self.ask_agent.is_none() {
            return;
        }
        let cmd = std::mem::take(&mut self.open_agent_menu).then_some(SetOpenCommand::Bool(true));
        let popup = Popup::from_response(new)
            .id(Id::NULL.with("agent-menu"))
            .open_memory(cmd)
            .align(RectAlign::TOP_START)
            .gap(6.0)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .frame(popup_frame(8, MENU_RIM));
        let shown = popup.show(|ui| {
            ui.set_min_width(176.0);
            for kind in [AgentKind::Claude, AgentKind::Codex] {
                if menu_item(ui, kind.name(), None, None, true).clicked() {
                    self.actions.push(Action::StartWith(kind));
                    ui.close();
                }
            }
        });
        if shown.is_none() && !self.actions.iter().any(|a| matches!(a, Action::StartWith(_))) {
            self.ask_agent = None;
        }
    }

    fn draw_terminal_side(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, TERMINAL_BG);
        if self.settings_open {
            self.draw_settings(ui);
            return;
        }
        let header = top_band(rect, HEADER_H);
        let body = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), rect.max);
        ui.scope_builder(UiBuilder::new().max_rect(header), |ui| self.draw_header(ui));
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| self.draw_pane(ui));
    }

    fn draw_header(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let p = ui.painter();
        p.rect_filled(rect, 0, SURFACE_BG);
        p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, HAIRLINE));
        let active = self.active_pane();
        let (status, title, has_id) = match active {
            Some(pane) => {
                let m = lock(&self.model);
                let name = m.sidebar.by_pane(pane.id()).map_or(pane.title.clone(), |r| r.name().to_string());
                (Some(pane.status), name, pane.shared.session_id().is_some())
            }
            None => (None, String::new(), false),
        };
        let has_pane = active.is_some();
        let inner = rect.shrink2(Vec2::new(14.0, 0.0));
        dot(p, Pos2::new(inner.left() + 3.5, inner.center().y), status_color(sidebar::status_key(status)));
        // Right to left: ✕, Branch from here, Attach files…, then (later phases) the image, page
        // and doc chips; the title and the status strip take what is left.
        let mut title_right = inner.right();
        ui.scope_builder(UiBuilder::new().max_rect(inner).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            let close = quiet_button(ui, "✕", has_pane, Vec2::new(9.0, 4.0), true)
                .on_hover_text("Close this session. It drops back into the list below Open.");
            if close.clicked() {
                self.actions.push(Action::CloseActive);
            }
            ui.add_space(8.0);
            let fork = quiet_button(ui, "Branch from here", has_pane && has_id, Vec2::new(10.0, 5.0), true)
                .on_hover_text("Copies this conversation into a new session. The original is untouched.");
            if fork.clicked() {
                self.actions.push(Action::Branch);
            }
            ui.add_space(8.0);
            let attach = quiet_button(ui, "Attach files…", has_pane, Vec2::new(10.0, 5.0), true).on_hover_text(
                "Puts the paths of the files you pick at the prompt, for Claude to read. Ctrl+V does the same for files copied in Explorer.",
            );
            if attach.clicked() {
                self.actions.push(Action::Attach);
            }
            ui.add_space(6.0);
            // Slot: doc, page and images chips (phase 7).
            title_right = ui.min_rect().left();
        });
        let x = inner.left() + 7.0 + 10.0;
        let g = elided(ui, &title, ui_font(12.0), TEXT_MUTED, title_right - x - 10.0);
        let y = rect.center().y - g.size().y / 2.0;
        ui.painter().galley(Pos2::new(x, y), g, TEXT_MUTED);
        // Slot: the status strip (HeadStrip, phase 4) follows the title.
    }

    fn draw_pane(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let ctx = ui.ctx().clone();
        let Some(i) = self.active.and_then(|id| self.panes.iter().position(|p| p.id() == id)) else {
            let text = if lock(&self.model).roots_exist { EMPTY_PANES } else { sidebar::EMPTY_STATE };
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, ui_font(13.0), TEXT_FAINT);
            return;
        };
        let font = self.font.clone();
        let focus_now = std::mem::take(&mut self.focus_terminal);
        let list_focus = self.list_focus;
        let pane = &mut self.panes[i];
        let (resp, out) = ui
            .push_id(widget::view_id(pane.id()), |ui| widget::show(ui, &pane.shared.term, &mut pane.view, &font, true))
            .inner;
        if focus_now || (!resp.has_focus() && !list_focus && ctx.memory(|m| m.focused().is_none())) {
            resp.request_focus();
        }
        if resp.has_focus() {
            self.list_focus = false;
        }
        for bytes in &out.input {
            pane.shared.type_keys(bytes);
        }
        if let Some(size) = out.resized {
            pane.shared.resize(size);
        }
        if let Some(text) = out.copy {
            ctx.copy_text(text);
        }
        if out.paste_requested {
            let p = &self.panes[i];
            self.paste(p);
        }
    }

    /// Placeholder for the Settings page (phase 7 fills it). It takes the terminal side's
    /// place, panes hidden but running.
    fn draw_settings(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let p = ui.painter();
        p.text(rect.left_top() + Vec2::new(36.0, 28.0), Align2::LEFT_TOP, "Settings", semibold(21.0), MENU_HOVER_FG);
        p.text(
            rect.left_top() + Vec2::new(36.0, 66.0),
            Align2::LEFT_TOP,
            "Settings arrive in a later build of the Rust port.",
            ui_font(12.5),
            MUTED_AA,
        );
        let close = Rect::from_min_size(Pos2::new(rect.right() - 50.0, rect.top() + 12.0), Vec2::new(40.0, 30.0));
        ui.scope_builder(UiBuilder::new().max_rect(close), |ui| {
            if quiet_button(ui, "✕", true, Vec2::new(9.0, 4.0), true).on_hover_text("Close (Esc)").clicked() {
                self.actions.push(Action::CloseSettings);
            }
        });
    }

    /// The 7 px grab band with the hairline down its middle; hover or drag shows 2 px Working.
    fn splitter(&mut self, ui: &mut Ui, rect: Rect, total: f32) {
        let resp = ui.interact(rect, Id::NULL.with("splitter"), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal);
        if resp.dragged() {
            self.sidebar_w += resp.drag_delta().x;
        }
        self.sidebar_w = clamp_sidebar(self.sidebar_w, total);
        let x = rect.center().x;
        if resp.hovered() || resp.dragged() {
            ui.painter().vline(x, rect.y_range(), Stroke::new(2.0, WORKING));
        } else {
            ui.painter().vline(x, rect.y_range(), Stroke::new(1.0, HAIRLINE));
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let term_focused = self.active.is_some() && !self.list_focus && ctx.memory(|m| m.focused().is_some())
            && !ctx.memory(|m| m.focused() == Some(Id::NULL.with("search")))
            && self.rename.is_none();
        // Ctrl+, opens Settings, Esc closes it; inside a terminal these keys are the agent's.
        let (comma, esc, f2, enter) = ctx.input(|i| {
            (
                i.modifiers.command_only() && i.key_pressed(Key::Comma),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::F2),
                i.key_pressed(Key::Enter),
            )
        });
        if comma && (!term_focused || self.settings_open) {
            self.actions.push(Action::OpenSettings);
        }
        if esc && self.settings_open {
            self.actions.push(Action::CloseSettings);
        }
        if self.list_focus && self.rename.is_none() {
            if let Some(id) = self.selected.clone() {
                if f2 {
                    self.actions.push(Action::StartRename(id));
                } else if enter {
                    self.actions.push(Action::Open(id));
                }
            }
        }
    }

    fn handle_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AppEvent::Status(id, s) => {
                    let mut was = None;
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id() == id) {
                        was = Some(p.status);
                        p.status = s;
                    }
                    if was.is_some() {
                        lock(&self.model).sidebar.set_pane_status(id, s);
                    }
                    // Island (phase 5): notices from (was, s) — NeedsInput -> NeedsYou, Error ->
                    // Error, Done after Working -> Done, else cleared; SessionWorking on Working.
                    let _ = (was, PaneStatus::Working);
                }
            }
        }
    }

    fn set_visibility(&self) {
        for p in &self.panes {
            let v = self.shown && !self.settings_open && self.active == Some(p.id());
            p.shared.visible.store(v, Ordering::Relaxed);
        }
    }
}

fn clamp_sidebar(w: f32, total: f32) -> f32 {
    let max = SIDEBAR_MAX.min(total - SPLITTER_W - TERMINAL_MIN).max(SIDEBAR_MIN);
    w.clamp(SIDEBAR_MIN, max)
}

fn semibold_mono(size: f32) -> egui::FontId {
    // The tag is mono semibold in C#; the mono family has no semibold face here.
    mono(size)
}

/// Section header (Open / Today / Earlier): chevron, label 11.5 semibold, count 11 faint.
fn section_header(ui: &mut Ui, label: &str, count: usize, open: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    let p = ui.painter();
    let hovered = resp.hovered();
    if hovered {
        p.rect_filled(rect, 0, ROW_HOVER);
    }
    let cy = rect.center().y;
    chevron(p, Pos2::new(rect.left() + 10.0 + 3.0, cy), open, if hovered { TEXT_MUTED } else { TEXT_FAINT });
    p.text(Pos2::new(rect.left() + 24.0, cy), Align2::LEFT_CENTER, label, semibold(11.5), TEXT_PRIMARY);
    p.text(Pos2::new(rect.right() - 12.0, cy), Align2::RIGHT_CENTER, count.to_string(), ui_font(11.0), TEXT_FAINT);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Folder sub-header: margin 26,10,10,4; name 11 TextMuted and the count.
fn folder_header(ui: &mut Ui, name: &str, count: usize) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 29.0), Sense::hover());
    let y = rect.top() + 10.0;
    let c = ui.painter().layout_no_wrap(count.to_string(), ui_font(11.0), TEXT_FAINT);
    let cw = c.size().x;
    ui.painter().galley(Pos2::new(rect.right() - 10.0 - cw, y), c, TEXT_FAINT);
    let g = elided(ui, name, ui_font(11.0), TEXT_MUTED, rect.width() - 26.0 - 10.0 - cw - 8.0);
    ui.painter().galley(Pos2::new(rect.left() + 26.0, y), g, TEXT_MUTED);
}

/// TinyCombo: 11 px muted text and a small down chevron, RaisedBg on hover.
fn tiny_combo(ui: &mut Ui, text: &str) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), ui_font(11.0), TEXT_MUTED);
    let size = Vec2::new(g.size().x + 14.0 + 14.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4, RAISED_BG);
    }
    let gy = rect.center().y - g.size().y / 2.0;
    let gx = rect.left() + 7.0;
    let tw = g.size().x;
    p.galley(Pos2::new(gx, gy), g, TEXT_MUTED);
    let c = Pos2::new(gx + tw + 10.0, rect.center().y + 1.0);
    let s = Stroke::new(1.3, TEXT_FAINT);
    p.line_segment([c + Vec2::new(-4.0, -2.0), c + Vec2::new(0.0, 2.0)], s);
    p.line_segment([c + Vec2::new(0.0, 2.0), c + Vec2::new(4.0, -2.0)], s);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn tiny_popup(button: &egui::Response, add: impl FnOnce(&mut Ui)) {
    Popup::menu(button).align(RectAlign::BOTTOM_START).gap(3.0).frame(popup_frame(5, HAIRLINE)).show(|ui| {
        ui.set_min_width(button.rect.width().max(100.0));
        add(ui);
    });
}

/// TinyComboItem: 11 px, padding 8,5, hover #2B313C, the chosen one in Working.
fn tiny_item(ui: &mut Ui, label: &str, chosen: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 3, POPUP_HOVER);
    }
    let c = if chosen {
        WORKING
    } else if hovered {
        TEXT_PRIMARY
    } else {
        TEXT_MUTED
    };
    ui.painter().text(Pos2::new(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, label, ui_font(11.0), c);
    resp
}

/// The folder picker's full-width QuietButton.
fn quiet_button_wide(ui: &mut Ui, text: &str, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 4, RAISED_BG);
    }
    ui.painter().rect_stroke(rect, 4, Stroke::new(1.0, HAIRLINE), StrokeKind::Inside);
    let c = if hovered { TEXT_PRIMARY } else { TEXT_MUTED };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, ui_font(12.0), c);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

impl eframe::App for ClayoApp {
    /// Runs while hidden too: pane events, the reveal request, and close-only-hides.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        if self.reveal.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            self.shown = true;
        }
        // The window's X only hides: sessions keep running, the island brings it back.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.shown = false;
            Popup::close_all(ctx);
        }
        self.set_visibility();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.first_frame_done {
            self.first_frame_done = true;
            crate::mark("first frame");
        }
        let ctx = ui.ctx().clone();
        self.shown = ctx.input(|i| i.viewport().visible()).unwrap_or(true) && self.shown;
        self.handle_keys(&ctx);
        let full = ui.max_rect();
        self.sidebar_w = clamp_sidebar(self.sidebar_w, full.width());
        let side = Rect::from_min_size(full.min, Vec2::new(self.sidebar_w, full.height()));
        let split = Rect::from_min_size(Pos2::new(side.right(), full.top()), Vec2::new(SPLITTER_W, full.height()));
        let term = Rect::from_min_max(Pos2::new(split.right(), full.top()), full.max);
        ui.scope_builder(UiBuilder::new().max_rect(side), |ui| self.draw_sidebar(ui));
        ui.painter().rect_filled(split, 0, Color32::TRANSPARENT);
        self.splitter(ui, split, full.width());
        ui.scope_builder(UiBuilder::new().max_rect(term), |ui| self.draw_terminal_side(ui));
        self.run_actions(&ctx, frame);
        self.set_visibility();
        // Ages ("3 min ago") and the Today bucket move with the clock.
        ctx.request_repaint_after(std::time::Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_width_stays_in_its_range_and_leaves_the_terminal_its_minimum() {
        assert_eq!(clamp_sidebar(100.0, 1280.0), SIDEBAR_MIN);
        assert_eq!(clamp_sidebar(900.0, 1280.0), SIDEBAR_MAX);
        assert_eq!(clamp_sidebar(500.0, 760.0), 760.0 - 7.0 - 360.0);
        assert_eq!(clamp_sidebar(300.0, 400.0), SIDEBAR_MIN);
    }
}
