use coco_macro::ComponentExt;
use crossterm::event::KeyEvent;
use ratatui::{Frame, prelude::*};

use super::{Action, Component};
use crate::{
    components::Persistable,
    error::*,
    global,
    session::{self, Session},
};

#[derive(ComponentExt)]
#[component(type_id = "input")]
pub struct Input<'a> {
    pub textarea: tui_textarea::TextArea<'a>,
    pub cursor_style: Style,
}

impl Default for Input<'_> {
    fn default() -> Self {
        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_cursor_line_style(Style::reset());
        let cursor_style = textarea.cursor_style();
        Self {
            textarea,
            cursor_style,
        }
    }
}

impl Input<'_> {
    /// Clears all content from the input and returns the deleted text.
    ///
    /// This method selects all text in the textarea, cuts it to clipboard,
    /// and returns the content that was removed.
    pub fn clear(&mut self) -> String {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.yank_text()
    }
}

impl Persistable for Input<'static> {
    fn save(&self) -> Session {
        session::save(self.textarea.lines().join("\n"))
    }

    fn load(session: Session) -> Result<Self> {
        let text: String = session::load(session)?;
        let mut inst = Self::default();
        inst.textarea.set_yank_text(text);
        inst.textarea.paste();
        inst.textarea.set_yank_text("");
        Ok(inst)
    }
}

impl Component for Input<'static> {
    fn update(&mut self, action: &Action) {
        match action {
            Action::Blur => {
                self.textarea.set_cursor_style(Style::reset());
            }
            Action::Focus => {
                self.textarea.set_cursor_style(self.cursor_style);
            }
            _ => (), // ignore
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        let cursor = self.textarea.cursor();
        // Signal dirty if text changed or cursor position changed
        if self.textarea.input(key.to_owned()) || self.textarea.cursor() != cursor {
            global::signal_dirty();
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        frame.render_widget(&self.textarea, area);
        Ok(())
    }
}
