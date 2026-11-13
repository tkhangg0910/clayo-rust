//! A pane's status light, inferred from the terminal output and (Codex) the transcript.
//! Port of TerminalPane.Scan/Answers/CountsAsWork/FromTimer/FromTurn (PORTING.md §4.5).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneStatus {
    Starting,
    Working,
    /// The agent asked something and is blocked until you answer.
    NeedsInput,
    /// Quiet with nothing outstanding: the turn is over.
    Done,
    /// The agent printed an API failure.
    Error,
    /// The pane's shell ended.
    Exited,
}
