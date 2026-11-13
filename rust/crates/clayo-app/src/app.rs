//! The eframe application.

use crate::pane::{AppEvent, Pane, Ticker, Waker};
use crate::win;
use clayo_core::agent::claude::ClaudeAgent;
use clayo_core::launch::{plan, LaunchMode};
use clayo_core::paths::{claude_dir, data_dir, OsEnv};
use clayo_core::security::ask_rules;
use clayo_core::settings::{self, ClayoSettings};
use clayo_core::status::relay;
use clayo_term::input::{bracketed_path, paste_bytes};
use clayo_term::widget::{self, TermFont};
use clayo_term::GridSize;
use egui::{Color32, Frame};
use std::sync::mpsc::{channel, Receiver};

const BG: Color32 = Color32::from_rgb(0x14, 0x17, 0x1c);

pub fn run() -> i32 {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("C L A Y O")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([760.0, 420.0]);
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
        glow_options: egui_glow_config(),
        clipboard_shortcuts: false,
        persist_window: false,
        ..Default::default()
    };
    crate::mark("run_native");
    match eframe::run_native("Clayo", opts, Box::new(|cc| Ok(Box::new(ClayoApp::new(cc))))) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn egui_glow_config() -> eframe::egui_glow::GlowConfiguration {
    eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() }
}

pub struct ClayoApp {
    settings: ClayoSettings,
    panes: Vec<Pane>,
    active: Option<usize>,
    next_id: u64,
    waker: Waker,
    rx: Receiver<AppEvent>,
    ticker: Ticker,
    font: TermFont,
    first_frame_done: bool,
}

impl ClayoApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::mark("app new (window + GL ready)");
        cc.egui_ctx.set_fonts(crate::fonts::definitions());
        crate::mark("fonts");
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = BG;
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        #[cfg(windows)]
        if let Ok(h) = raw_window_handle::HasWindowHandle::window_handle(cc) {
            if let raw_window_handle::RawWindowHandle::Win32(w) = h.as_raw() {
                win::dark_caption(w.hwnd.get());
            }
        }
        let (tx, rx) = channel();
        let env = OsEnv;
        let settings = ClayoSettings::load(&settings::default_path(&data_dir(&env)));
        let mut app = Self {
            settings,
            panes: Vec::new(),
            active: None,
            next_id: 1,
            waker: Waker { ctx: cc.egui_ctx.clone(), tx },
            rx,
            ticker: Ticker::start(),
            font: TermFont::default(),
            first_frame_done: false,
        };
        let folder = std::env::current_dir().unwrap_or_default();
        app.start_claude(&folder.to_string_lossy());
        crate::mark("pane started");
        app
    }

    fn start_claude(&mut self, folder: &str) {
        let env = OsEnv;
        let data = data_dir(&env);
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let relay = relay::write_settings(&data, &claude_dir(&env), &exe, &ask_rules::claude_ask(&self.settings));
        let agent = ClaudeAgent::for_this_user(&env, self.settings.claude_path.as_deref(), relay.as_deref());
        let Ok(mut p) = plan(&agent, LaunchMode::New, folder, None, &env) else { return };
        // Debug builds: run something else in place of the agent (rendering checks, benches).
        if cfg!(debug_assertions) {
            if let Ok(cmd) = std::env::var("CLAYO_DEV_AGENT_CMD") {
                p.agent_command = cmd;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let pane = Pane::start(id, &p, GridSize { cols: 120, rows: 40 }, None, self.waker.clone(), self.ticker.clone());
        self.panes.push(pane);
        self.active = Some(self.panes.len() - 1);
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
                    } else if f.contains(' ') {
                        out.extend(format!("\"{f}\" ").into_bytes());
                    } else {
                        out.extend(format!("{f} ").into_bytes());
                    }
                }
                pane.shared.type_keys(&out);
            }
            win::Clip::Nothing => {}
        }
    }
}

impl eframe::App for ClayoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.first_frame_done {
            self.first_frame_done = true;
            crate::mark("first frame");
        }
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AppEvent::Status(id, s) => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id() == id) {
                        p.status = s;
                    }
                }
            }
        }
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().frame(Frame::NONE.fill(BG)).show(ui, |ui| {
            let Some(i) = self.active else { return };
            let font = self.font.clone();
            let pane = &mut self.panes[i];
            let (resp, out) = widget::show(ui, &pane.shared.term, &mut pane.view, &font, true);
            if !resp.has_focus() && ctx.memory(|m| m.focused().is_none()) {
                resp.request_focus();
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
        });
    }
}
