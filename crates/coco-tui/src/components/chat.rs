use code_combo::{Agent, Config};
use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    prelude::*,
    symbols::border,
    text::Line,
    widgets::{Block, Borders},
};
use tracing::debug;

use super::{
    Action, BotMessage, Combo, ComboAction, ComboEvent, Component, Event, Input, Message, Plain,
    Role,
};
use crate::global;

pub struct Chat<'a> {
    state: State,
    focus: Focus,
    agent: Agent,
    pub input: Input<'a>,
    pub messages: Vec<Message>,
}

#[derive(Default)]
enum State {
    #[default]
    Ready,
    Procesing,
    ComboDiscovering,
}

#[derive(Default, PartialEq)]
enum Focus {
    #[default]
    Input,
    Messages,
}

impl Chat<'_> {
    pub fn new(config: Config) -> Self {
        Self {
            state: State::default(),
            focus: Focus::default(),
            agent: Agent::new(config),
            input: Input::default(),
            messages: vec![],
        }
    }

    fn handle_combo_event(&mut self, event: &ComboEvent) {
        debug!(?event, "receive combo event");
        match event {
            ComboEvent::Discovering => {
                self.state = State::ComboDiscovering;
            }
            ComboEvent::Executing { .. } | ComboEvent::Output { .. } => {
                self.state = State::Procesing;
            }
            ComboEvent::Discovered { .. }
            | ComboEvent::Executed { .. }
            | ComboEvent::NotFound { .. } => {
                self.state = State::Ready;
            }
        }
    }
}

impl Component for Chat<'_> {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        let mut children: Vec<&mut dyn Component> = vec![];
        children.push(&mut self.input);
        children.extend(self.messages.iter_mut().map(|m| m as &mut dyn Component));
        Box::new(children.into_iter())
    }

    fn handle_event(&mut self, event: &Event) {
        // Override the default handle_event method to handle specific events manually
        match event {
            Event::Key(key) => {
                self.handle_key_event(key);
            }
            Event::Combo(combo) => {
                self.handle_combo_event(combo);
                // Combo events need to be handled by children components
                handle_component_event!(self, event);
            }
            Event::Ask => {
                self.state = State::Procesing;
            }
            Event::Answer(msgs) => {
                self.state = State::Ready;
                self.messages
                    .extend(msgs.iter().cloned().map(|msg| Message {
                        role: Role::Bot,
                        content: Box::new(match msg {
                            BotMessage::Plain(text) => Plain::new(text),
                            #[allow(unreachable_patterns)]
                            _ => unreachable!("unknown bot message type: {msg:?}"),
                        }),
                    }));
            }
            _ => {
                // Handle other kinds of events by default
                handle_component_event!(self, event);
            }
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        debug!(?key, "handling key event");

        match (key.modifiers, key.code) {
            (_, KeyCode::Tab) => {
                // TODO: no Tab control.
                // Esc -> selection mode, Up/Down -> Move Selection, Enter -> focus
                if self.focus == Focus::Input {
                    self.focus = Focus::Messages;
                    self.input.update(&Action::Blur);
                } else {
                    self.focus = Focus::Input;
                    self.input.update(&Action::Focus);
                }
            }
            (_, KeyCode::Enter) => {
                if matches!(self.state, State::Ready) {
                    let value = self.input.clear();
                    debug!(?value, "submiting");
                    self.messages.push(Message {
                        role: Role::User,
                        content: Box::new(Plain::new(value.clone())),
                    });

                    let tx = global::event_tx();
                    let mut agent = self.agent.clone();
                    tokio::task::spawn(async move {
                        let msg = code_combo::Message {
                            role: code_combo::MessageRole::User,
                            content: code_combo::MessageContent::Text(value.clone()),
                        };
                        tx.send(Event::Ask).unwrap();
                        let msgs = agent.chat(msg).await;
                        tx.send(Event::Answer(
                            msgs.into_iter()
                                .map(|m| {
                                    if let code_combo::MessageContent::Text(text) = m.content {
                                        BotMessage::Plain(text)
                                    } else {
                                        unreachable!("unknown content type: {:?}", m.content)
                                    }
                                })
                                .collect(),
                        ))
                        .unwrap();
                    });
                } else {
                    // TODO: Display an alert when input submission is not available
                }
            }
            _ => {
                match self.focus {
                    Focus::Input => self.input.handle_key_event(key),
                    Focus::Messages => {
                        // TODO: handle event
                    }
                }
            }
        }
    }

    fn update(&mut self, action: &Action) {
        debug!(?action, "updating");

        if let Action::Combo(ComboAction::Discover | ComboAction::Execute { .. }) = action {
            let combo = Combo::default();
            // Add a combo message to handle the current action and any subsequent actions.
            self.messages.push(Message {
                role: Role::User,
                content: Box::new(combo),
            });
            debug!("Combo message pushed");
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let vertical = Layout::vertical([Min(0), Length(1), Length(1), Length(1)]);
        let [area_messages, divider, area_input, bottom] = vertical.areas(area);

        let chunks = Layout::vertical(self.messages.iter().map(|m| Length(m.height() as u16)))
            .flex(Flex::End)
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
        frame.render_widget(
            Block::new()
                .borders(Borders::BOTTOM)
                .border_set(border::THICK)
                .title_bottom(Line::from(""))
                .title_bottom(
                    Line::from({
                        match self.state {
                            State::Ready => " [Ready] ".green(),
                            State::Procesing => " [Procesing] ".yellow(),
                            State::ComboDiscovering => " [Discovering] ".yellow(),
                        }
                        .bold()
                    })
                    .left_aligned(),
                ),
            bottom,
        );
        self.input.draw(frame, area_input)
    }
}
