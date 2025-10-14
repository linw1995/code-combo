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
