# Clayo (Rust)

A native Rust port of [Clayo](https://github.com/YogiOnCode/Clayo): every Claude Code and Codex
session in one Windows window, plus an island that tells you which one needs you. Same behaviour
as Clayo 1.0.6, without WebView2: one `clayo.exe`, no runtime to install.

**Credit.** Clayo was created by **Yogeswaran Amsavalli** ([YogiOnCode/Clayo](https://github.com/YogiOnCode/Clayo)),
MIT licensed. This port keeps that licence: see [`../LICENSE`](../LICENSE). The design, behaviour,
texts, mascot and island are the original author's; the Rust code reimplements them.

| | Rust port | C# 1.0.6 |
|---|---|---|
| Start to first frame | ~150 ms | ~1 s (8.8 s first start) |
| RAM, 1 pane | 33 MB | 80 MB + 144 MB WebView2 |
| RAM, 5 panes | 48 MB | 65 MB + 345 MB WebView2 |
| Install | one 18 MB exe | 135 MB, 260 files |

Full numbers and how they were taken: [`../BENCH.md`](../BENCH.md). What maps to what, and every
behaviour checked off against the C# source: [`../PORTING.md`](../PORTING.md).

## Requirements

- Windows 10 or 11, 64-bit.
- [Claude Code](https://docs.claude.com/en/docs/claude-code) and/or [Codex](https://github.com/openai/codex)
  (the first-run setup window finds them, or installs one in a PowerShell window you can watch).
- To build: Rust stable (1.85+) with the MSVC toolchain (`rustup default stable-x86_64-pc-windows-msvc`)
  and the Visual Studio C++ build tools. `git` on PATH is used at run time for the branch/diff in
  the pane header.

## Build

```powershell
cd rust
cargo build --release          # rust\target\release\clayo.exe
cargo test                     # all checks (the C# checks/*.cs, ported), ~430 tests
cargo clippy --all-targets -- -D warnings
```

Workspace layout:

| Crate | What it is |
|---|---|
| `clayo-core` | All logic with no UI: transcripts, sessions, status relay and store, settings, rules, island state machine, setup texts, login start, PATH, updater. Tested on its own. |
| `clayo-pty` | ConPTY: spawn, read thread, write, resize, non-blocking teardown. |
| `clayo-term` | VT parsing (`alacritty_terminal`), colours, key/mouse encoding, the egui terminal widget. |
| `clayo-island` | The island: a layered Win32 window that never takes focus, glow, mascot, screenshot picker, drop target. |
| `clayo-app` | `clayo.exe`: eframe/egui (OpenGL, Direct3D 12 fallback) main window, setup window, Settings. |
| `bench` | `clayo-bench` and the scripts behind BENCH.md (not part of clayo.exe). |

## Install

Build the release zip and install it with the same script as the C# build:

```powershell
.\rust\scripts\publish.ps1      # publish\clayo-<version>-win-x64.zip + .sha256 (uploads nothing)
```

Then either unzip it into `%LOCALAPPDATA%\Programs` (giving `%LOCALAPPDATA%\Programs\Clayo\clayo.exe`)
and run `clayo.exe`, or attach the zip and its `.sha256` to a GitHub release and use
[`scripts/install.ps1`](../scripts/install.ps1), which checks the SHA-256 and swaps the folder.
Keep `clayo.exe` in that folder: start at login and `clayo` in Explorer's address bar point at it.

Updates: the Update button asks GitHub for the latest release of the repo set at build time
(`CLAYO_UPDATE_REPO`, default `YogiOnCode/Clayo`). Before publishing Rust releases from a fork,
build with `$env:CLAYO_UPDATE_REPO = "<owner>/<repo>"`, or the button would install the upstream
C# release over this one.

## Moving from the C# build

Quit the C# Clayo from its gear menu, then install this one into the same folder. Your settings
(`%LOCALAPPDATA%\Clayo\settings.json`), custom session names, branch links and screenshots are read
as they are. The two builds share the single-instance lock, so they never run side by side.

## What Clayo touches

- Reads only: `%USERPROFILE%\.claude` (or `%CLAUDE_CONFIG_DIR%`) and `%USERPROFILE%\.codex` (or
  `%CODEX_HOME%`). It never writes there (`clayo-core/tests/invariants.rs`).
- Writes its own files in `%LOCALAPPDATA%\Clayo` only; registry: `HKCU\...\Run` value `Clayo` and
  the user `Path`, both from the setup window.
- Network: only the GitHub releases check.

## Differences from the C# build

Agreed in PORTING.md §6: HTML pages a session writes open in your browser instead of beside the
terminal (D3); Markdown docs show no remote images (D4); emoji are monochrome (D7); "Also apply to
Codex" is read-only, because Clayo does not write into `~/.codex` (D1); the terminal cursor stops
blinking after 5 s of quiet (D14).

## Debugging hooks

Environment variables read by `clayo.exe`: `CLAYO_STARTUP_LOG`, `CLAYO_FRAME_LOG`, `CLAYO_BENCH_TYPE`,
`CLAYO_BENCH_AGENT_CMD` (benchmarks), and in debug builds `CLAYO_PTY_LOG`, `CLAYO_DEV_AGENT_CMD`,
`CLAYO_DEV_SETTINGS`, `CLAYO_DEV_RENDERER`. Point `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and
`LOCALAPPDATA` at temp folders to try a build without touching your real setup.
