//! The setup window's words and states, from what the agent detector found and the "Use" ticks.
//! No UI, so the tests can drive it.

use std::sync::LazyLock;

use regex::Regex;

use crate::agent::{AgentInfo, AgentKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentRowState {
    Looking,
    Ready,
    SignIn,
    Missing,
    Installing,
    SigningIn,
}

/// One agent's row: its pill and the grey line under its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRowView {
    pub kind: AgentKind,
    pub state: AgentRowState,
    pub meta: String,
}

impl AgentRowView {
    pub fn new(kind: AgentKind, state: AgentRowState, meta: impl Into<String>) -> Self {
        Self {
            kind,
            state,
            meta: meta.into(),
        }
    }
}

/// Everything the setup window shows that depends on what was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupView {
    pub title: String,
    pub intro: String,
    pub claude: AgentRowView,
    pub codex: AgentRowView,
    pub show_picker: bool,
    pub options_enabled: bool,
    pub note: String,
    pub can_open: bool,
}

const LOOKING: &str = "Checking PATH and usual install folders";
const FOLLOW_ALONG: &str =
    "Follow along in the PowerShell window. Clayo checks again when it closes.";

/// While the detector runs.
pub fn checking() -> SetupView {
    SetupView {
        title: "Setting up Clayo".into(),
        intro: "Looking for Claude Code and Codex on this PC…".into(),
        claude: AgentRowView::new(AgentKind::Claude, AgentRowState::Looking, LOOKING),
        codex: AgentRowView::new(AgentKind::Codex, AgentRowState::Looking, LOOKING),
        show_picker: false,
        options_enabled: false,
        note: "Takes about a second.".into(),
        can_open: false,
    }
}

/// Install's PowerShell window is open. Nothing else can be done meanwhile; the other row stays
/// as it was.
pub fn installing(found: &SetupView, kind: AgentKind) -> SetupView {
    let meta = match kind {
        AgentKind::Claude => "Running the official installer…",
        AgentKind::Codex => "Running npm install -g @openai/codex…",
    };
    SetupView {
        title: format!("Installing {}", kind.name()),
        intro: FOLLOW_ALONG.into(),
        note: "Then sign in once, and Clayo is ready.".into(),
        ..waiting(
            found,
            AgentRowView::new(kind, AgentRowState::Installing, meta),
        )
    }
}

/// The same while Sign in's window is open.
pub fn signing_in(found: &SetupView, kind: AgentKind) -> SetupView {
    SetupView {
        title: format!("Signing in to {}", kind.name()),
        intro: FOLLOW_ALONG.into(),
        note: "Your browser may open to sign in.".into(),
        ..waiting(
            found,
            AgentRowView::new(
                kind,
                AgentRowState::SigningIn,
                "Finish in the PowerShell window",
            ),
        )
    }
}

fn waiting(found: &SetupView, row: AgentRowView) -> SetupView {
    let mut v = found.clone();
    match row.kind {
        AgentKind::Claude => v.claude = row,
        AgentKind::Codex => v.codex = row,
    }
    v.show_picker = false;
    v.options_enabled = false;
    v.can_open = false;
    v
}

pub fn state_of(a: &AgentInfo) -> AgentRowState {
    match (&a.exe, a.signed_in) {
        (None, _) => AgentRowState::Missing,
        (Some(_), true) => AgentRowState::Ready,
        (Some(_), false) => AgentRowState::SignIn,
    }
}

/// What the window says once the detector is done.
pub fn from(claude: &AgentInfo, codex: &AgentInfo, use_claude: bool, use_codex: bool) -> SetupView {
    use AgentRowState::{Ready, SignIn};
    let cs = state_of(claude);
    let xs = state_of(codex);
    let any_ready = cs == Ready || xs == Ready;
    let can_open = (cs == Ready && use_claude) || (xs == Ready && use_codex);

    let (title, intro, note) = if cs == Ready && xs == Ready {
        (
            "Clayo is ready".to_string(),
            "Found both agents. Their sessions share one sidebar.".to_string(),
            "Change any of this later in Settings.".to_string(),
        )
    } else if any_ready {
        let (ready, other, other_state) = if cs == Ready {
            (claude, codex, xs)
        } else {
            (codex, claude, cs)
        };
        let other_name = other.kind.name();
        if other_state == SignIn {
            (
                "Clayo is ready".to_string(),
                format!("{other_name} is installed but not signed in. Clayo can start without it."),
                format!("Sign in to {other_name} any time, then Check again."),
            )
        } else {
            (
                "Clayo is ready".to_string(),
                format!(
                    "{} is all Clayo needs. {other_name} is optional.",
                    ready.kind.name()
                ),
                format!("Add {other_name} any time from Settings."),
            )
        }
    } else if cs == SignIn || xs == SignIn {
        let intro = if cs == xs {
            "Both agents are installed but not signed in.".to_string()
        } else {
            let kind = if cs == SignIn {
                AgentKind::Claude
            } else {
                AgentKind::Codex
            };
            format!("{} is installed but not signed in.", kind.name())
        };
        (
            "Almost there".to_string(),
            intro,
            "Sign in, then Check again.".to_string(),
        )
    } else {
        (
            "Install one agent to start".to_string(),
            "Clayo runs your coding agent in its panes. Either one works.".to_string(),
            "Install installs it for you, in a window you can watch.".to_string(),
        )
    };

    SetupView {
        title,
        intro,
        claude: row(claude, cs, any_ready),
        codex: row(codex, xs, any_ready),
        show_picker: cs == Ready && xs == Ready,
        options_enabled: can_open,
        note,
        can_open,
    }
}

fn row(a: &AgentInfo, state: AgentRowState, any_ready: bool) -> AgentRowView {
    let version = short_version(a.version.as_deref());
    let meta = match state {
        AgentRowState::Ready => format!("{version} · {}", count(a.sessions, a.projects)),
        AgentRowState::SignIn => format!("{version} · Sign in once, then Clayo can use it"),
        // Missing: optional when the other one works, else who makes it. Codex installs through npm.
        _ if a.kind == AgentKind::Claude => {
            (if any_ready { "Optional." } else { "Anthropic" }).to_string()
        }
        _ => match (any_ready, a.npm.is_some()) {
            (true, false) => "Optional. Node.js not found.",
            (true, true) => "Optional. Needs Node.js.",
            (false, false) => "OpenAI · Node.js not found",
            (false, true) => "OpenAI · needs Node.js",
        }
        .to_string(),
    };
    AgentRowView::new(a.kind, state, meta)
}

static VERSION_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+(\.\d+)+").expect("version regex"));

/// "2.1.288 (Claude Code)" and "codex-cli 0.160.0" both become "v2.1.288" style; a version with
/// no number stays as it is, and none is "".
pub fn short_version(version: Option<&str>) -> String {
    match version {
        None => String::new(),
        Some(v) => match VERSION_NUMBER.find(v) {
            Some(m) => format!("v{}", m.as_str()),
            None => v.to_string(),
        },
    }
}

fn count(sessions: u32, projects: u32) -> String {
    if sessions == 0 {
        return "No sessions yet".into();
    }
    let s = if sessions == 1 { "session" } else { "sessions" };
    let p = if projects == 1 { "project" } else { "projects" };
    format!("{sessions} {s} in {projects} {p}")
}
