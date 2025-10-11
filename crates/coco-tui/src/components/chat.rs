use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::*;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use tracing::debug;

use super::{Action, Component};

use super::Input;

#[derive(Default)]
pub struct Chat<'a> {
    focus: Focus,
    pub input: Input<'a>,
}

#[derive(Default, PartialEq)]
enum Focus {
    #[default]
    Input,
    Messages,
}

impl Component for Chat<'_> {
    fn handle_key_event(&mut self, key: KeyEvent) -> Option<Action> {
        debug!(?key, "handling key event");

        match (key.modifiers, key.code) {
            (_, KeyCode::Tab) => {
                if self.focus == Focus::Input {
                    self.focus = Focus::Messages;
                    self.input.update(Action::Blur).unwrap()
                } else {
                    self.focus = Focus::Input;
                    self.input.update(Action::Focus).unwrap()
                }
            }
            _ => {
                match self.focus {
                    Focus::Input => self.input.handle_key_event(key),
                    Focus::Messages => {
                        // TODO: handle event
                        None
                    }
                }
            }
        }
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        debug!(?action, "updating");
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let vertical = Layout::vertical([Min(0), Length(1), Length(2)]);
        let [_main_area, divider, input_area] = vertical.areas(area);

        frame.render_widget(
            Block::new()
                .borders(Borders::TOP)
                .title_bottom(Line::from(""))
                .title_bottom(Line::from(vec![
                    " Toggle Focus ".into(),
                    "<Tab> ".blue().bold(),
                ])),
            divider,
        );
        self.input.draw(frame, input_area)
    }
}
