use code_combo::{TextEdit, ToolUse};
use tokio::time::Instant;

use crate::session::{PersistentSessionMetadata, Session};

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    Render,

    Combo(ComboAction),
    Tool(ToolAction),
    Session(SessionAction),
    CommandPalette(CommandPaletteAction),
    SubmitPrompt(String),

    Blur,
    Focus,
}

const SAVE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

impl Action {
    /// Create a Session action with schedule save
    pub fn schedule_session_save() -> Self {
        SessionAction::ScheduleSave {
            save_at: Instant::now() + SAVE_DELAY,
        }
        .into()
    }

    /// Restore last Session
    pub fn restore_last_session() -> Self {
        SessionAction::RestoreLastSession.into()
    }

    /// Restore a Session
    pub fn restore_session(s: Session) -> Self {
        SessionAction::RestoreSession(s).into()
    }
}

#[derive(Debug, Clone)]
pub enum ComboAction {
    Discover,
    Execute { name: String },
}

impl From<ComboAction> for Action {
    fn from(value: ComboAction) -> Self {
        Self::Combo(value)
    }
}

#[derive(Debug, Clone)]
pub enum ToolAction {
    Grant(ToolUse),
    GrantSession(ToolUse),
    Cancel(ToolUse),
    ApplyTextEdit {
        id: String,
        name: String,
        edit: TextEdit,
        context_radius: usize,
        hunk_idx: usize,
        is_rejecting: bool,
    },
}

impl From<ToolAction> for Action {
    fn from(value: ToolAction) -> Self {
        Self::Tool(value)
    }
}

#[derive(Debug, Clone)]
pub enum CommandPaletteAction {
    NewSession,
    Transcript,
    RestoreSession(PersistentSessionMetadata),
    SwitchTheme(String),
    Shell,
}

impl From<CommandPaletteAction> for Action {
    fn from(value: CommandPaletteAction) -> Self {
        Self::CommandPalette(value)
    }
}

#[derive(Debug, Clone)]
pub enum SessionAction {
    ScheduleSave { save_at: Instant },
    RestoreLastSession,
    RestoreSession(Session),
}

impl From<SessionAction> for Action {
    fn from(value: SessionAction) -> Self {
        Self::Session(value)
    }
}
