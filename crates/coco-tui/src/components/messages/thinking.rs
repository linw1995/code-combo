use coco_macro::{ComponentExt, ContentComponentExt};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    prelude::Rect,
    widgets::{Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};

use super::{Component, Content, ShortcutHints, fold::FoldState};
use crate::{
    components::{ContentComponent, Persistable},
    error::*,
    global,
    session::{self, Session},
};

#[derive(Serialize, Deserialize)]
struct ThinkingState {
    text: String,
    fold_state: FoldState,
}

#[derive(ComponentExt, ContentComponentExt)]
#[component(type_id = "thinking")]
pub struct Thinking {
    state: ThinkingState,
}

impl Thinking {
    pub fn new(text: String) -> Self {
        Self {
            state: ThinkingState {
                text,
                fold_state: FoldState::Expanded,
            },
        }
    }

    pub fn collapse(&mut self) {
        self.state.fold_state.collapse();
    }

    pub fn toggle(&mut self) {
        self.state.fold_state = self.state.fold_state.toggle();
    }

    pub fn is_collapsed(&self) -> bool {
        self.state.fold_state.is_collapsed()
    }

    fn display_text(&self) -> &str {
        &self.state.text
    }
}

impl Persistable for Thinking {
    fn save(&self) -> Session {
        session::save(&self.state)
    }

    fn load(session: Session) -> Result<Self> {
        let state: ThinkingState = session::load(session)?;
        Ok(Self { state })
    }
}

impl Component for Thinking {
    fn handle_key_event(&mut self, key: &KeyEvent) {
        if matches!(key.code, KeyCode::Char('r' | 'R' | 'z' | 'Z')) {
            self.toggle();
            global::signal_dirty();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if self.is_collapsed() {
            return Ok(());
        }
        let theme = global::theme();
        let text = self.display_text().to_string();
        let paragraph = Paragraph::new(text)
            .style(theme.ui.thinking_text)
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        Ok(())
    }
}

impl Content for Thinking {
    fn height(&self, width: u16) -> usize {
        if width == 0 {
            return 0;
        }
        if self.is_collapsed() {
            return 0;
        }
        let paragraph = Paragraph::new(self.display_text().to_string()).wrap(Wrap { trim: false });
        paragraph.line_count(width)
    }

    fn shortcut_hints(&self) -> ShortcutHints {
        if self.is_collapsed() {
            return ShortcutHints::default();
        }
        let mut hints = ShortcutHints::default();
        hints.push_visible(&[("Fold", "r")]);
        hints
    }
}

impl ContentComponent for Thinking {}
