//! The two agents Clayo runs, and how to find and start them.

pub mod claude;
pub mod codex;
pub mod detect;
pub mod helper;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
        }
    }

    /// Sidebar tag when both agents are in use.
    pub fn tag(self) -> &'static str {
        match self {
            AgentKind::Claude => "CC",
            AgentKind::Codex => "CX",
        }
    }
}
