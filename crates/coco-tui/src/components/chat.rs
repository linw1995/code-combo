use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::*;
use ratatui::symbols::border;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use tracing::debug;

use super::{Action, Component, Content, Input, Message, Role};

#[derive(Default)]
pub struct Chat<'a> {
    focus: Focus,
    pub input: Input<'a>,
    pub messages: Vec<Message>,
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
                // TODO: no Tab control.
                // Esc -> selection mode, Up/Down -> Move Selection, Enter -> focus
                if self.focus == Focus::Input {
                    self.focus = Focus::Messages;
                    self.input.update(Action::Blur).unwrap()
                } else {
                    self.focus = Focus::Input;
                    self.input.update(Action::Focus).unwrap()
                }
            }
            (_, KeyCode::Enter) => {
                let value = self.input.clear();
                debug!(?value, "submiting");
                self.messages.push(Message {
                    role: Role::User,
                    content: Content::Plain(value),
                });
                None
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
        let [area_messages, divider, area_input] = vertical.areas(area);

        let chunks = Layout::vertical(self.messages.iter().map(|m| Length(m.height() as u16)))
            .split(area_messages);
        for (idx, message) in self.messages.iter_mut().enumerate() {
            message.draw(frame, chunks[idx]).unwrap();
        }

        frame.render_widget(
            Block::new()
                .borders(Borders::BOTTOM)
                .border_set(border::THICK)
                .title_bottom(Line::from(""))
                .title_bottom(Line::from(vec![
                    " Toggle Focus ".into(),
                    " <Tab> ".blue().bold(),
                ]))
                .title_bottom(Line::from(vec![
                    " Submit ".into(),
                    " <Enter> ".blue().bold(),
                ])),
            divider,
        );
        self.input.draw(frame, area_input)
    }
}
