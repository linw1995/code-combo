#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    Render,

    Combo(ComboAction),

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
        Action::Combo(value)
    }
}
