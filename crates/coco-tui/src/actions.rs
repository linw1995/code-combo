use code_combo::{TextEdit, ToolUse};
use tokio::time::Instant;

use crate::session::Session;

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    Render,

    Combo(ComboAction),
    Tool(ToolAction),
    Session(SessionAction),
    Command(String),

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

    /// Create a Session action with immediate save
    pub fn save_session_now() -> Self {
        SessionAction::SaveNow.into()
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
pub enum SessionAction {
    ScheduleSave { save_at: Instant },
    SaveNow,
    RestoreLastSession,
    RestoreSession(Session),
}

impl From<SessionAction> for Action {
    fn from(value: SessionAction) -> Self {
        Self::Session(value)
    }
}
