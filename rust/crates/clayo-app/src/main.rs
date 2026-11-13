//! `clayo.exe`: every Claude Code and Codex session in one window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts;
mod island_host;
mod model;
mod pane;
mod status;
mod status_view;
mod theme;
mod win;

/// Startup timing marks, written to the file named by `CLAYO_STARTUP_LOG` (for BENCH.md).
pub fn mark(what: &str) {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    let t0 = *T0.get_or_init(std::time::Instant::now);
    if let Some(path) = std::env::var_os("CLAYO_STARTUP_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{:>7.1} ms  {what}", t0.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

use clayo_core::paths::{data_dir, OsEnv};
use clayo_core::status::relay;

fn main() {
    // The status line relay runs on every status refresh of every Claude pane: it must be the
    // very first thing, before any window, font or mutex (PORTING.md §4.1).
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == relay::ARG) {
        let pass = args.next().is_some_and(|a| a == relay::PASS_ARG);
        let dir = relay::status_dir(&data_dir(&OsEnv));
        let code = relay::run(pass, std::io::stdin().lock(), std::io::stdout().lock(), &dir);
        std::process::exit(code);
    }

    // Children must start as top-level sessions, not as children of a Claude Code session
    // Clayo itself may have been launched from.
    mark("main");
    clayo_core::launch::scrub_inherited_session();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(std::path::Path::to_path_buf)).unwrap_or_default();
    let windir = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR")).unwrap_or_else(|| "C:\\Windows".into());
    let system_dir = std::path::Path::new(&windir).join("System32");
    let start = clayo_core::cli::parse(&args, &cwd, &exe_dir, &system_dir, |p| std::path::Path::new(p).is_dir());
    let code = app::run(app::Startup { background: start.background, folder: start.folder });
    // Hard exit: no stray thread (a PTY reader, a watcher) may keep the process alive.
    std::process::exit(code);
}
