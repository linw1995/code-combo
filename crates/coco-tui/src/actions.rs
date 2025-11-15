use code_combo::ToolUse;

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
}

impl From<ToolAction> for Action {
    fn from(value: ToolAction) -> Self {
        Self::Tool(value)
    }
}
