use color_eyre::eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    prelude::*,
    symbols::border,
    widgets::{Block, Borders, Padding},
};
use tracing::debug;

use super::{Action, Component};

pub struct Input<'a> {
    blur: bool,
    state: State,
    pub textarea: tui_textarea::TextArea<'a>,
}

impl Default for Input<'_> {
    fn default() -> Self {
        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_cursor_line_style(Style::reset());
        Self {
            blur: false,
            state: State::Ready,
            textarea,
        }
    }
}

#[derive(Default, Debug, PartialEq)]
#[allow(dead_code)]
enum State {
    #[default]
    Ready,
    Procesing,
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

impl Component for Input<'_> {
    fn handle_key_event(&mut self, key: KeyEvent) -> Option<Action> {
        debug!(?key, "handling key event");
        self.textarea.input(key);
        None
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        debug!(?action, "updating");
        match action {
            Action::Focus => {
                self.blur = false;
            }
            Action::Blur => {
                self.blur = true;
            }
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let block = Block::new()
            .borders(Borders::BOTTOM)
            .border_set(border::THICK)
            .title_bottom(Line::from(""))
            .title_bottom(
                Line::from({
                    let mut state = match self.state {
                        State::Ready => " [Ready] ".green(),
                        State::Procesing => " [Procesing] ".yellow(),
                    }
                    .bold();
                    if self.blur {
                        state = state.gray()
                    }
                    state
                })
                .left_aligned(),
            )
            .padding(Padding::left(1));

        frame.render_widget(&self.textarea, block.inner(area));
        frame.render_widget(block, area);
        Ok(())
    }
}
