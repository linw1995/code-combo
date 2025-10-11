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

#[derive(Default)]
#[allow(dead_code)]
pub struct Input<'a> {
    blur: bool,
    state: State,
    pub textarea: tui_textarea::TextArea<'a>,
}

#[derive(Default, Debug, PartialEq)]
#[allow(dead_code)]
enum State {
    #[default]
    Ready,
    Procesing,
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
        let mut block = Block::new()
            .borders(Borders::BOTTOM)
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
            .padding(Padding::left(1))
            .border_set(border::THICK);

        if !self.blur {
            let instructions = Line::from(vec![" Submit ".into(), "<Enter> ".blue().bold()]);
            block = block.title_bottom(instructions.left_aligned());
        }

        frame.render_widget(&self.textarea, block.inner(area));
        frame.render_widget(block, area);
        Ok(())
    }
}
