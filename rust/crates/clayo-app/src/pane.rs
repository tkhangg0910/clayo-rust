//! One session pane: a shell under ConPTY running the agent, its terminal, and its status.
//!
//! The PTY read thread parses output into the terminal and updates the status tracker, so a
//! pane keeps working while hidden or while the window is closed (eframe stops calling `ui`
//! then). Status changes and ticks travel to the UI as [`AppEvent`]s, handled in `App::logic`,
//! which eframe keeps calling for a hidden window; the shared [`Ticker`] thread replaces the
//! C# per-pane 400 ms timer with one-shot deadlines (D13).

use chrono::Utc;
use clayo_core::agent::AgentKind;
use clayo_core::launch::LaunchPlan;
use clayo_core::pane_status::{PaneStatus, PaneStatusTracker};
use clayo_core::sessions::TurnState;
use clayo_pty::{PtyConfig, PtyEvent, PtyProcess};
use clayo_term::widget::ViewState;
use clayo_term::{GridSize, Terminal};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

/// What background threads tell the UI.
#[derive(Debug)]
pub enum AppEvent {
    Status(u64, PaneStatus),
    /// The island asks the main window for something.
    Island(crate::island_host::IslandCmd),
}

/// Wakes the UI from any thread.
#[derive(Clone)]
pub struct Waker {
    pub ctx: egui::Context,
    pub tx: Sender<AppEvent>,
}

impl Waker {
    pub fn send(&self, e: AppEvent) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

/// The part of a pane that background threads share.
pub struct PaneShared {
    pub id: u64,
    pub term: Arc<Terminal>,
    pub tracker: Mutex<PaneStatusTracker>,
    pty: Mutex<Option<PtyProcess>>,
    /// Only the active pane of a shown window is drawn, and only it asks for repaints.
    pub visible: AtomicBool,
    /// None for a Codex pane until its transcript is found (the sessions thread binds it).
    session_id: Mutex<Option<String>>,
    /// Typed at the prompt on the first Done (Open a file).
    insert_on_done: Mutex<Option<String>>,
    /// Screenshots sent to a Codex pane (path, when), for the image chip (phase 7): Codex
    /// reads them where they are, so nothing else records them.
    pub codex_images: Mutex<Vec<(PathBuf, chrono::DateTime<Utc>)>>,
    waker: Waker,
    ticker: Ticker,
}

impl PaneShared {
    /// Write to the PTY (keys, pastes, prefill).
    pub fn write(&self, bytes: &[u8]) {
        if let Ok(p) = self.pty.lock() {
            if let Some(p) = p.as_ref() {
                p.write(bytes);
            }
        }
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|s| s.clone())
    }

    pub fn set_session_id(&self, id: Option<String>) {
        if let Ok(mut s) = self.session_id.lock() {
            *s = id;
        }
    }

    /// Types paths at the prompt, unsent (InsertPaths).
    pub fn insert_paths<S: AsRef<str>>(&self, paths: &[S]) {
        let text = clayo_core::text::insert_paths(paths);
        if !text.is_empty() {
            self.write(text.as_bytes());
        }
    }

    /// Types `text` once the session first settles at its prompt (its first Done): earlier, the
    /// keys would reach a CLI still starting up, or a trust prompt.
    pub fn insert_on_first_done(&self, text: String) {
        if let Ok(mut t) = self.insert_on_done.lock() {
            *t = Some(text);
        }
    }

    /// A Codex transcript's turn: working / done are exact there.
    pub fn show_turn(&self, turn: TurnState, at: chrono::DateTime<Utc>) {
        let s = self.tracker.lock().ok().and_then(|mut t| t.on_turn(turn, at));
        self.status_changed(s);
    }

    fn emit(&self, s: PaneStatus) {
        if s == PaneStatus::Done {
            if let Some(text) = self.insert_on_done.lock().ok().and_then(|mut t| t.take()) {
                self.write(text.as_bytes());
            }
        }
        self.waker.send(AppEvent::Status(self.id, s));
    }

    fn status_changed(&self, s: Option<PaneStatus>) {
        if let Some(s) = s {
            self.emit(s);
        }
        self.ticker.poke();
    }

    /// Keys typed by the user: written, and they may answer a question.
    pub fn type_keys(&self, bytes: &[u8]) {
        self.write(bytes);
        let s = self.tracker.lock().ok().and_then(|mut t| t.on_key(bytes, Utc::now()));
        self.status_changed(s);
    }

    pub fn resize(&self, size: GridSize) {
        if let Ok(p) = self.pty.lock() {
            if let Some(p) = p.as_ref() {
                p.resize(size.cols, size.rows);
            }
        }
        if let Ok(mut t) = self.tracker.lock() {
            t.on_resize(Utc::now());
        }
    }

    fn tick(&self) {
        let out = match self.tracker.lock() {
            Ok(mut t) => t.on_tick(Utc::now()),
            Err(_) => return,
        };
        if let Some(text) = out.prefill {
            self.write(text.as_bytes());
        }
        if let Some(s) = out.status {
            self.emit(s);
        }
    }

    /// Kills the shell (and the agent in it); clayo-pty tears the console down off this thread.
    pub fn kill(&self) {
        if let Ok(mut p) = self.pty.lock() {
            p.take();
        }
    }
}

/// A pane the UI owns.
pub struct Pane {
    pub shared: Arc<PaneShared>,
    pub agent: AgentKind,
    pub cwd: PathBuf,
    /// Shown in the header until the row has a name.
    pub title: String,
    pub status: PaneStatus,
    pub view: ViewState,
}

impl Pane {
    /// Start the shell for `plan`, then type the agent command 400 ms later so PowerShell is
    /// ready for it (TP:446-459).
    pub fn start(
        id: u64,
        plan: &LaunchPlan,
        title: String,
        size: GridSize,
        prefill: Option<String>,
        waker: Waker,
        ticker: Ticker,
    ) -> Self {
        let now = Utc::now();
        let mut tracker = PaneStatusTracker::new(plan.agent == AgentKind::Codex, now);
        tracker.set_prefill(prefill);
        // The terminal's own replies (device status, colour queries) go to this pane's PTY.
        let link: Arc<OnceLock<Weak<PaneShared>>> = Arc::new(OnceLock::new());
        let reply = link.clone();
        let term = Arc::new(Terminal::new(
            size,
            Arc::new(move |b: &[u8]| {
                if let Some(p) = reply.get().and_then(Weak::upgrade) {
                    p.write(b);
                }
            }),
        ));
        let shared = Arc::new(PaneShared {
            id,
            term: term.clone(),
            tracker: Mutex::new(tracker),
            pty: Mutex::new(None),
            visible: AtomicBool::new(false),
            session_id: Mutex::new(plan.expected_session_id.clone()),
            insert_on_done: Mutex::new(None),
            codex_images: Mutex::new(Vec::new()),
            waker: waker.clone(),
            ticker: ticker.clone(),
        });

        let _ = link.set(Arc::downgrade(&shared));
        let weak: Weak<PaneShared> = Arc::downgrade(&shared);
        let cfg = PtyConfig {
            command_line: plan.shell_command_line.clone(),
            cwd: Some(PathBuf::from(&plan.working_directory)),
            cols: size.cols,
            rows: size.rows,
        };
        let sink = move |e: PtyEvent| {
            let Some(p) = weak.upgrade() else { return };
            match e {
                PtyEvent::Output(bytes) => {
                    trace(&bytes);
                    p.term.feed(&bytes);
                    let s = p.tracker.lock().ok().and_then(|mut t| t.on_output(&bytes, Utc::now()));
                    p.status_changed(s);
                    if p.visible.load(Ordering::Relaxed) {
                        p.waker.ctx.request_repaint();
                    }
                }
                PtyEvent::Exited(_) => {
                    p.term.feed(b"\r\n\x1b[38;5;244m[shell exited]\x1b[0m");
                    let s = p.tracker.lock().ok().and_then(|mut t| t.on_exit());
                    p.status_changed(s);
                    if p.visible.load(Ordering::Relaxed) {
                        p.waker.ctx.request_repaint();
                    }
                }
            }
        };
        let mut exited = false;
        match clayo_pty::spawn(&cfg, sink) {
            Ok(proc_) => {
                *shared.pty.lock().expect("fresh mutex") = Some(proc_);
                let typed = Arc::downgrade(&shared);
                let cmd = plan.agent_command.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(400));
                    if let Some(p) = typed.upgrade() {
                        p.write(format!("{cmd}\r").as_bytes());
                        if let Ok(mut t) = p.tracker.lock() {
                            t.on_command_typed(Utc::now());
                        }
                        p.ticker.poke();
                    }
                });
            }
            Err(e) => {
                term.feed(format!("Couldn't start the shell: {e}\r\n").as_bytes());
                if let Ok(mut t) = shared.tracker.lock() {
                    t.on_exit();
                }
                exited = true;
            }
        }
        ticker.add(&shared);
        Self {
            shared,
            agent: plan.agent,
            cwd: PathBuf::from(&plan.working_directory),
            title,
            status: if exited { PaneStatus::Exited } else { PaneStatus::Starting },
            view: ViewState::default(),
        }
    }

    pub fn id(&self) -> u64 {
        self.shared.id
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.shared.kill();
    }
}

/// One thread for every pane's status deadlines: it sleeps until the earliest one, or until a
/// pane pokes it because its deadline moved. No pane, or no pending deadline, means no wakeups.
#[derive(Clone)]
pub struct Ticker {
    inner: Arc<(Mutex<TickerState>, Condvar)>,
}

#[derive(Default)]
struct TickerState {
    panes: Vec<Weak<PaneShared>>,
    poked: bool,
}

impl Ticker {
    pub fn start() -> Self {
        let t = Self { inner: Arc::new((Mutex::new(TickerState::default()), Condvar::new())) };
        let me = t.clone();
        std::thread::Builder::new()
            .name("pane ticker".into())
            .spawn(move || me.run())
            .expect("spawn ticker");
        t
    }

    fn add(&self, p: &Arc<PaneShared>) {
        if let Ok(mut s) = self.inner.0.lock() {
            s.panes.push(Arc::downgrade(p));
        }
        self.poke();
    }

    pub fn poke(&self) {
        if let Ok(mut s) = self.inner.0.lock() {
            s.poked = true;
        }
        self.inner.1.notify_one();
    }

    fn run(&self) {
        let (lock, cv) = &*self.inner;
        loop {
            let now = Utc::now();
            let mut due = Vec::new();
            let mut next: Option<chrono::DateTime<Utc>> = None;
            {
                let Ok(mut s) = lock.lock() else { return };
                s.panes.retain(|w| w.strong_count() > 0);
                for p in s.panes.iter().filter_map(Weak::upgrade) {
                    let d = p.tracker.lock().ok().and_then(|t| t.next_deadline(now));
                    match d {
                        Some(d) if d <= now => due.push(p),
                        Some(d) => next = Some(next.map_or(d, |n| n.min(d))),
                        None => {}
                    }
                }
            }
            for p in &due {
                p.tick();
            }
            if !due.is_empty() {
                continue;
            }
            let Ok(mut s) = lock.lock() else { return };
            if s.poked {
                s.poked = false;
                continue;
            }
            let wait = next.map(|n| (n - Utc::now()).to_std().unwrap_or_default());
            s = match wait {
                Some(w) => cv.wait_timeout(s, w).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0),
                None => cv.wait(s).unwrap_or_else(|e| e.into_inner()),
            };
            s.poked = false;
        }
    }
}

/// Debug builds only: append raw PTY output to the file named by `CLAYO_PTY_LOG`.
fn trace(bytes: &[u8]) {
    if cfg!(debug_assertions) {
        if let Some(path) = std::env::var_os("CLAYO_PTY_LOG") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = f.write_all(bytes);
            }
        }
    }
}
