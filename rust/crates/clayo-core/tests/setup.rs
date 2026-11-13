//! Port of checks/setup.cs (the SetupScreen part; the AgentHelper commands are tested with the
//! agent helper): the setup window's states from made-up detector results.

use clayo_core::agent::{AgentInfo, AgentKind};
use clayo_core::setup::{
    checking, from, installing, short_version, signing_in, AgentRowState, AgentRowView,
};
use AgentRowState::*;

fn claude_ready() -> AgentInfo {
    AgentInfo {
        kind: AgentKind::Claude,
        exe: Some(r"C:\c.exe".into()),
        version: Some("2.1.287 (Claude Code)".into()),
        signed_in: true,
        data_folder: String::new(),
        sessions: 42,
        projects: 7,
        npm: None,
    }
}

fn codex_ready() -> AgentInfo {
    AgentInfo {
        kind: AgentKind::Codex,
        exe: Some(r"C:\x.cmd".into()),
        version: Some("codex-cli 0.160.0".into()),
        signed_in: true,
        data_folder: String::new(),
        sessions: 9,
        projects: 3,
        npm: Some(r"C:\npm.cmd".into()),
    }
}

fn claude_missing() -> AgentInfo {
    AgentInfo {
        kind: AgentKind::Claude,
        exe: None,
        version: None,
        signed_in: false,
        data_folder: String::new(),
        sessions: 0,
        projects: 0,
        npm: None,
    }
}

fn codex_missing() -> AgentInfo {
    AgentInfo {
        exe: None,
        version: None,
        signed_in: false,
        sessions: 0,
        projects: 0,
        ..codex_ready()
    }
}

fn codex_no_node() -> AgentInfo {
    AgentInfo {
        npm: None,
        ..codex_missing()
    }
}

fn codex_sign_in() -> AgentInfo {
    AgentInfo {
        signed_in: false,
        ..codex_ready()
    }
}

fn claude_sign_in() -> AgentInfo {
    AgentInfo {
        signed_in: false,
        ..claude_ready()
    }
}

#[test]
fn checking_state() {
    let v = checking();
    assert_eq!(v.title, "Setting up Clayo");
    assert_eq!(
        (v.claude.state, v.codex.state),
        (Looking, Looking),
        "both looking"
    );
    assert!(!v.can_open, "can't open");
    assert!(!v.options_enabled, "options greyed");
}

#[test]
fn both_ready() {
    let v = from(&claude_ready(), &codex_ready(), true, true);
    assert_eq!(v.title, "Clayo is ready");
    assert_eq!(
        v.intro,
        "Found both agents. Their sessions share one sidebar."
    );
    assert_eq!(v.claude.meta, "v2.1.287 · 42 sessions in 7 projects");
    assert_eq!(v.codex.meta, "v0.160.0 · 9 sessions in 3 projects");
    assert!(v.show_picker, "picker shows");
    assert!(v.can_open);
    assert_eq!(v.note, "Change any of this later in Settings.");
    assert!(
        !from(&claude_ready(), &codex_ready(), false, false).can_open,
        "both unticked"
    );
    assert!(
        from(&claude_ready(), &codex_ready(), false, true).can_open,
        "one unticked"
    );
}

#[test]
fn claude_only_and_its_mirror() {
    let v = from(&claude_ready(), &codex_missing(), true, true);
    assert_eq!(
        v.intro,
        "Claude Code is all Clayo needs. Codex is optional."
    );
    assert_eq!(v.codex.meta, "Optional. Needs Node.js.");
    assert!(!v.show_picker, "no picker");
    assert_eq!(v.note, "Add Codex any time from Settings.");
    assert_eq!(
        from(&claude_ready(), &codex_no_node(), true, true)
            .codex
            .meta,
        "Optional. Node.js not found."
    );
    assert!(
        !from(&claude_ready(), &codex_missing(), false, true).can_open,
        "claude unticked"
    );

    let v = from(&claude_missing(), &codex_ready(), true, true);
    assert_eq!(
        v.intro,
        "Codex is all Clayo needs. Claude Code is optional."
    );
    assert_eq!(v.claude.meta, "Optional.");
    assert!(v.can_open);
}

#[test]
fn sign_in() {
    let v = from(&claude_missing(), &codex_sign_in(), true, true);
    assert_eq!(v.title, "Almost there");
    assert_eq!(v.intro, "Codex is installed but not signed in.");
    assert_eq!(v.codex.state, SignIn);
    assert_eq!(
        v.codex.meta,
        "v0.160.0 · Sign in once, then Clayo can use it"
    );
    assert!(!v.can_open);
    assert_eq!(v.note, "Sign in, then Check again.");
    assert_eq!(
        from(&claude_sign_in(), &codex_sign_in(), true, true).intro,
        "Both agents are installed but not signed in."
    );
    let v = from(&claude_ready(), &codex_sign_in(), true, true);
    assert!(v.can_open, "one ready, one signed out: opens");
    assert_eq!(
        v.intro,
        "Codex is installed but not signed in. Clayo can start without it."
    );
    assert_eq!(v.note, "Sign in to Codex any time, then Check again.");
}

#[test]
fn none_installed() {
    let v = from(&claude_missing(), &codex_no_node(), true, true);
    assert_eq!(v.title, "Install one agent to start");
    assert_eq!(
        v.intro,
        "Clayo runs your coding agent in its panes. Either one works."
    );
    assert_eq!(v.claude.meta, "Anthropic");
    assert_eq!(v.codex.meta, "OpenAI · Node.js not found");
    assert_eq!(
        from(&claude_missing(), &codex_missing(), true, true)
            .codex
            .meta,
        "OpenAI · needs Node.js"
    );
    assert!(!v.can_open);
    assert!(!v.options_enabled);
    assert_eq!(
        v.note,
        "Install installs it for you, in a window you can watch."
    );
}

#[test]
fn installing_changes_only_that_row() {
    let v = installing(
        &from(&claude_missing(), &codex_missing(), true, true),
        AgentKind::Claude,
    );
    assert_eq!(v.title, "Installing Claude Code");
    assert_eq!(
        v.claude,
        AgentRowView::new(
            AgentKind::Claude,
            Installing,
            "Running the official installer…"
        )
    );
    assert_eq!(v.codex.state, Missing, "codex row as it was");
    assert!(!v.can_open);
    assert!(!v.options_enabled);
    assert_eq!(
        v.intro,
        "Follow along in the PowerShell window. Clayo checks again when it closes."
    );
    assert_eq!(v.note, "Then sign in once, and Clayo is ready.");

    let v = installing(
        &from(&claude_ready(), &codex_missing(), true, true),
        AgentKind::Codex,
    );
    assert!(
        !v.can_open,
        "beside a ready claude: still can't open meanwhile"
    );
    assert_eq!(v.codex.meta, "Running npm install -g @openai/codex…");
}

#[test]
fn signing_in_state() {
    let v = signing_in(
        &from(&claude_missing(), &codex_sign_in(), true, true),
        AgentKind::Codex,
    );
    assert_eq!(v.title, "Signing in to Codex");
    assert_eq!(v.codex.state, SigningIn);
    assert_eq!(v.codex.meta, "Finish in the PowerShell window");
    assert_eq!(v.note, "Your browser may open to sign in.");
}

#[test]
fn small_things() {
    let one = AgentInfo {
        sessions: 1,
        projects: 1,
        ..claude_ready()
    };
    assert_eq!(
        from(&one, &codex_missing(), true, true).claude.meta,
        "v2.1.287 · 1 session in 1 project"
    );
    let none = AgentInfo {
        sessions: 0,
        projects: 0,
        ..claude_ready()
    };
    assert_eq!(
        from(&none, &codex_missing(), true, true).claude.meta,
        "v2.1.287 · No sessions yet"
    );
    assert_eq!(short_version(Some("dev build")), "dev build");
    assert_eq!(short_version(None), "");
}
