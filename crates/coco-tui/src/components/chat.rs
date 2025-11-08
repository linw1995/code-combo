use code_combo::{Agent, Config, ExecuteOutput, ToolUse};
use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    prelude::*,
    symbols::border,
    text::Line,
    widgets::{Block, Borders},
};
use tracing::{debug, warn};

use super::{
    Action, AnswerEvent, AskEvent, BotMessage, Combo, ComboAction, ComboEvent, Component, Event,
    Input, Message, Plain, Role,
};
use crate::{
    actions::ToolAction,
    components::{Content, ContentComponent, Tool, shortcuts_desc},
    global,
};

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

#[derive(Debug, Default, PartialEq)]
enum Focus {
    #[default]
    Input,
    InputBlur,
    Messages(usize),
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

    fn input_focus(&mut self) {
        self.focus = Focus::Input;
        self.input.update(&Action::Focus);
    }

    fn input_blur(&mut self) {
        if self.focus == Focus::Input {
            self.input.update(&Action::Blur);
        }
        self.focus = Focus::InputBlur;
    }

    fn move_focus_up(&mut self) {
        match self.focus {
            Focus::InputBlur if !self.messages.is_empty() => {
                self.focus = Focus::Messages(self.messages.len() - 1);
            }
            Focus::Messages(idx) if idx > 0 => self.focus = Focus::Messages(idx - 1),
            _ => (), // ignore
        }
    }

    fn move_focus_down(&mut self) {
        if let Focus::Messages(idx) = self.focus
            && idx < self.messages.len() - 1
        {
            self.focus = Focus::Messages(idx + 1);
        } else {
            self.focus = Focus::InputBlur;
        }
    }

    fn on_submit(&mut self) {
        if matches!(self.state, State::Ready) {
            let value = self.input.clear();
            debug!(?value, "submiting");
            self.messages
                .push(Message::user(Plain::new(value.clone()).boxed()));
            tokio::task::spawn(task_chat(
                self.agent.clone(),
                code_combo::Content::Text(value),
            ));
        } else {
            // TODO: Display an alert when input submission is not available
        }
    }

    fn block_bottom_with_shortcuts_desc<'a>(&self, mut block: Block<'a>) -> Block<'a> {
        block = block.title_bottom(Line::from(""));
        match self.focus {
            Focus::Input => block
                .title_bottom(shortcuts_desc(&[("Blur", "Esc")]))
                .title_bottom(shortcuts_desc(&[("Submit", "CR")])),
            Focus::InputBlur => block
                .title_bottom(shortcuts_desc(&[("Focus", "CR")]))
                .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")])),
            Focus::Messages(idx) => {
                let component = &self.messages[idx].content;
                if component.actionable() {
                    block = component.block_bottom_with_shortcuts_desc(block);
                }
                block.title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")]))
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
            Event::Ask(AskEvent::Bot) => {
                self.state = State::Procesing;
            }
            Event::Answer(AnswerEvent::Bot(msgs)) => {
                self.state = State::Ready;
                self.messages.extend(msgs.iter().cloned().map(|msg| {
                    Message::bot(match msg {
                        BotMessage::Plain(text) => Plain::new(text).boxed(),
                        BotMessage::ToolUse { id, name, input } => {
                            Tool::new_use(id, name, input).boxed()
                        }
                    })
                }));
            }
            _ => {
                // Handle other kinds of events by default
                handle_component_event!(self, event);
            }
        }
    }

    fn handle_key_event(&mut self, key: &KeyEvent) {
        use Focus::*;
        use KeyCode::*;
        use KeyModifiers as KM;
        debug!(?key, "handling key event");

        match (&self.focus, key.modifiers, key.code) {
            (Input, KM::NONE, Enter) => self.on_submit(),
            (Input, KM::NONE, Esc) => self.input_blur(),
            (Input, _, _) => self.input.handle_key_event(key),

            (InputBlur, KM::NONE, Enter) => self.input_focus(),

            (Messages(_) | InputBlur, KM::NONE, Char('k')) => self.move_focus_up(),
            (Messages(_), KM::NONE, Char('j')) => self.move_focus_down(),

            (Messages(idx), _, _) if self.messages[*idx].actionable() => {
                self.messages[*idx].handle_key_event(key);
            }
            (Messages(_), KM::NONE, Esc) => self.input_blur(),

            (InputBlur | Messages(_), _, _) => {
                warn!(?key, ?self.focus, "unknown key event");
            }
        }
    }

    fn update(&mut self, action: &Action) {
        debug!(?action, "updating");

        match action {
            Action::Combo(ComboAction::Discover | ComboAction::Execute { .. }) => {
                let combo = Combo::default();
                self.messages.push(Message {
                    role: Role::User,
                    content: Box::new(combo),
                });
                debug!("Combo message pushed");
            }
            Action::Tool(action) => match action {
                ToolAction::Grant(tool_use) => {
                    self.agent.grant_once(&tool_use.id, &tool_use.name);
                    tokio::task::spawn(task_tool_use(self.agent.clone(), tool_use.to_owned()));
                }
            },
            _ => (),
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
            let mut block = Block::new().borders(Borders::LEFT);
            block = if self.focus == Focus::Messages(idx) {
                block.border_set(border::THICK)
            } else {
                block
                    .border_set(border::PLAIN)
                    .border_style(Style::default().dark_gray())
            };
            let rect = chunks[idx];
            frame.render_widget(&block, rect);
            message.draw(frame, block.inner(rect)).unwrap();
        }

        let block = Block::new()
            .borders(Borders::BOTTOM)
            .border_set(border::THICK);
        frame.render_widget(self.block_bottom_with_shortcuts_desc(block), divider);

        let mut block = Block::new().borders(Borders::BOTTOM);
        block = if !matches!(self.focus, Focus::Messages(_)) {
            block.border_set(border::THICK).border_style(Style::reset())
        } else {
            block
                .border_set(border::PLAIN)
                .border_style(Style::default().dark_gray())
        };
        frame.render_widget(
            block.title_bottom(Line::from("")).title_bottom(
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

async fn task_chat(mut agent: Agent, content: code_combo::Content) {
    let tx = global::event_tx();

    let msg = code_combo::Message::user(content);
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();

    let msg = agent.chat(msg).await;

    let mut to_execute: Vec<code_combo::ToolUse> = vec![];
    tx.send(
        AnswerEvent::Bot(match msg.content {
            code_combo::Content::Text(text) => {
                vec![BotMessage::Plain(text)]
            }
            code_combo::Content::Multiple(blocks) => {
                to_execute.extend(blocks.iter().filter_map(|b| {
                    if let code_combo::Block::ToolUse(tool_use) = b {
                        Some(tool_use.clone())
                    } else {
                        None
                    }
                }));
                blocks
                    .into_iter()
                    .map(|m| match m {
                        code_combo::Block::Text { text } => BotMessage::Plain(text),
                        code_combo::Block::ToolUse(code_combo::ToolUse { id, name, input }) => {
                            BotMessage::ToolUse { id, name, input }
                        }
                        _ => unreachable!("unknown content type: {:?}", m),
                    })
                    .collect()
            }
        })
        .into(),
    )
    .unwrap();

    // Parallel execution
    let handles = to_execute
        .into_iter()
        .map(|tool_use| {
            let agent = agent.clone();
            tokio::task::spawn(task_tool_use(agent, tool_use))
        })
        .collect::<Vec<_>>();
    futures::future::join_all(handles).await;
}

async fn task_tool_use(mut agent: Agent, tool_use: ToolUse) {
    let tx = global::event_tx();
    let code_combo::ToolUse { id, name, input } = tool_use;
    // It will be executed if permission check pass
    // TODO: push ToolResult message
    match agent.execute(&id, &name, input).await {
        ExecuteOutput::AskPermission => tx.send(AskEvent::ToolUsePermission(id).into()).unwrap(),
        ExecuteOutput::Granted(output) => tx
            .send(AnswerEvent::ToolResult { id, output }.into())
            .unwrap(),
        ExecuteOutput::Denied => (),
    }
}
