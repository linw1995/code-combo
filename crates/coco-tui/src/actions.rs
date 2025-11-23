use code_combo::{TextEdit, ToolUse};

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    Render,

    Combo(ComboAction),
    Tool(ToolAction),

    Blur,
    Focus,
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
