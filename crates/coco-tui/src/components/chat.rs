use code_combo::{
    Agent, Block as ChatBlock, Config, Content as ChatContent, Message as ChatMessage, Output,
    TextEdit, ToolUse,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    prelude::*,
    symbols::border,
    text::Line,
    widgets::{Block, Borders},
};

use throbber_widgets_tui::{Throbber, ThrobberState};
use tracing::{debug, warn};

use super::{
    Action, AnswerEvent, AskEvent, BotMessage, Combo, ComboAction, ComboEvent, Component, Content,
    ContentComponent, Event, Input, Message, Messages, Plain, Role, Tool, ToolAction,
    shortcuts_desc,
};
use crate::{
    error::*,
    global::{self, State},
};

pub struct Chat<'a> {
    state: State<ChatState>,
    focus: State<Focus>,
    agent: Agent,

    input: Input<'a>,
    messages: Messages,
    pending_chats: Vec<ChatBlock>,
    indicator: ThrobberState,
}

#[derive(Default, Clone)]
enum ChatState {
    #[default]
    Ready,
    Procesing,
    ComboDiscovering,
}

impl std::fmt::Display for ChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready => f.write_str("Ready"),
            Self::Procesing => f.write_str("Procesing"),
            Self::ComboDiscovering => f.write_str("Discovering"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
enum Focus {
    #[default]
    Input,
    InputBlur,
    Messages,
}

impl Chat<'_> {
    pub fn new(config: Config) -> Self {
        Self {
            state: State::default(),
            focus: State::default(),
            agent: Agent::new(config),
            input: Input::default(),
            messages: Messages::default(),
            pending_chats: vec![],
            indicator: ThrobberState::default(),
        }
    }

    fn handle_combo_event(&mut self, event: &ComboEvent) {
        debug!(?event, "receive combo event");
        match event {
            ComboEvent::Discovering => {
                *self.state.write() = ChatState::ComboDiscovering;
            }
            ComboEvent::Executing { .. } | ComboEvent::Output { .. } => {
                *self.state.write() = ChatState::Procesing;
            }
            ComboEvent::Executed { starter, .. } => {
                let combo = starter.combo.as_ref().unwrap();
                let content = self.build_user_content(ChatContent::Text(combo.to_markdown()));
                tokio::task::spawn(task_chat(self.agent.clone(), content));
            }
            ComboEvent::Discovered { .. } | ComboEvent::NotFound { .. } => {
                *self.state.write() = ChatState::Ready;
            }
        }
    }

    fn update_focus(&mut self, new_focus: Focus) {
        let focus = self.focus.read();
        if focus == &new_focus {
            return;
        }
        debug!(?focus, ?new_focus, "update focus");
        if focus == &Focus::Input {
            self.input.update(&Action::Blur);
        }
        if new_focus == Focus::Input {
            self.input.update(&Action::Focus);
        }
        *self.focus.write() = new_focus
    }

    /// Combines pending ToolResults (e.g., User Cancelled) with user instructions.
    ///
    /// This ensures the LLM doesn't react to tool results without explicit user instructions.
    /// Tool results are queued and combined with the next user message to provide context.
    fn build_user_content(&mut self, content: ChatContent) -> ChatContent {
        if self.pending_chats.is_empty() {
            content
        } else {
            let mut blocks = std::mem::take(&mut self.pending_chats);
            ChatContent::Multiple(match content {
                ChatContent::Text(text) => {
                    blocks.push(ChatBlock::Text { text });
                    blocks
                }
                ChatContent::Multiple(parts) => {
                    blocks.extend(parts);
                    blocks
                }
            })
        }
    }

    fn on_submit(&mut self) {
        if matches!(self.state.read(), ChatState::Ready) {
            let value = self.input.clear();
            debug!(?value, "submiting");
            self.messages
                .push(Message::user(Plain::new(value.clone()).boxed()));
            let content = self.build_user_content(ChatContent::Text(value));
            tokio::task::spawn(task_chat(self.agent.clone(), content));
        } else {
            // TODO: Display an alert when input submission is not available
        }
    }

    fn block_bottom_with_shortcuts_desc<'a>(&self, mut block: Block<'a>) -> Block<'a> {
        block = block.title_bottom(Line::from(""));
        match self.focus.read() {
            Focus::Input => block
                .title_bottom(shortcuts_desc(&[("Blur", "Esc")]))
                .title_bottom(shortcuts_desc(&[("Submit", "CR")])),
            Focus::InputBlur => block
                .title_bottom(shortcuts_desc(&[("Focus", "CR")]))
                .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")])),
            Focus::Messages => {
                block = self.messages.block_bottom_with_shortcuts_desc(block);
                block
                    .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")]))
                    .title_bottom(shortcuts_desc(&[("Scroll Up", "C-y"), ("Down", "C-e")]))
                    .title_bottom(shortcuts_desc(&[("Scroll+ Up", "C-u"), ("Down", "C-d")]))
            }
        }
    }

    fn widget_state_indicator(&self) -> Line<'_> {
        let state = self.state.read();
        (match state {
            ChatState::Ready => Line::from(format!(" {state} ").green()),
            ChatState::Procesing | ChatState::ComboDiscovering => Line::from(vec![
                " ".into(),
                Throbber::default()
                    .throbber_set(throbber_widgets_tui::BRAILLE_EIGHT_DOUBLE)
                    .to_symbol_span(&self.indicator),
                format!("{state} ").yellow(),
            ]),
        })
        .bold()
    }
}

impl Component for Chat<'_> {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        let children: Vec<&mut dyn Component> = vec![&mut self.input, &mut self.messages];
        Box::new(children.into_iter())
    }

    fn on_tick(&mut self) {
        if matches!(
            self.state.get(),
            ChatState::Procesing | ChatState::ComboDiscovering
        ) {
            self.indicator.calc_next();
            global::signal_ditry();
        }
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
                *self.state.write() = ChatState::Procesing;
            }
            Event::Answer(AnswerEvent::Bot(msgs)) => {
                *self.state.write() = ChatState::Ready;
                self.messages.extend(msgs.iter().cloned().map(|msg| {
                    Message::bot(match msg {
                        BotMessage::Plain(text) => Plain::new(text).boxed(),
                        BotMessage::ToolUse(tool_use) => Tool::new(tool_use.to_owned()).boxed(),
                    })
                }));
            }
            Event::Ask(AskEvent::ToolUsePermission(_) | AskEvent::TextEdit { .. }) => {
                if let Some(idx) = self.messages.on_tool_event(event) {
                    // Move focus to tool use message when permission is required
                    self.update_focus(Focus::Messages);
                    self.messages.focus(idx);
                }
            }
            Event::Answer(AnswerEvent::ToolResult {
                id,
                is_error,
                output,
            }) => {
                if let Some(idx) = self.messages.on_tool_event(event)
                    && !is_error
                    && self.messages.selected_idx() == Some(idx)
                {
                    // Move focus back to Input if tool use success.
                    self.update_focus(Focus::Input);
                    self.messages.blur();
                }
                // Add ToolResult message to send execution result to LLM API Server
                // TODO: Allow user to retry if tool use fails.
                let content = ChatContent::Multiple(vec![code_combo::Block::ToolResult {
                    tool_use_id: id.clone(),
                    is_error: Some(*is_error),
                    content: output.try_into().unwrap(),
                }]);
                let content = self.build_user_content(content);
                tokio::task::spawn(task_chat(self.agent.clone(), content));
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

        match (self.focus.read(), key.modifiers, key.code) {
            // Focus switching
            (Input, KM::NONE, Esc) => self.update_focus(Focus::InputBlur),
            (InputBlur, KM::NONE, Enter) => self.update_focus(Focus::Input),
            (Messages, KM::NONE, Esc) if !self.messages.is_actionable() => {
                self.messages.blur();
                self.update_focus(Focus::InputBlur);
            }

            // Inputing
            (Input, KM::NONE, Enter) => self.on_submit(),
            (Input, _, _) => self.input.handle_key_event(key),

            // Navigation
            (InputBlur, KM::NONE, Char('k')) => {
                if self.messages.select_last() {
                    // Move focus to Messages if selecting the last message succeeds
                    self.update_focus(Focus::Messages);
                }
            }
            (Messages, KM::NONE, Char('k')) => {
                self.messages.select_prev();
            }
            (Messages, KM::NONE, Char('j')) => {
                if !self.messages.select_next() {
                    // Move focus to InputBlur when no more messages are available
                    self.messages.blur();
                    self.update_focus(Focus::InputBlur);
                }
            }
            // Scrolling
            (Messages, KM::CONTROL, Char('y')) => {
                self.messages.scroll_up(1);
            }
            (Messages, KM::CONTROL, Char('e')) => {
                self.messages.scroll_down(1);
            }
            (Messages, KM::CONTROL, Char('u')) => {
                self.messages.scroll_half_up();
            }
            (Messages, KM::CONTROL, Char('d')) => {
                self.messages.scroll_half_down();
            }

            // Handle actionable messages
            (Messages, _, _) => self.messages.handle_key_event(key),

            (InputBlur, _, _) => {
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
                ToolAction::Cancel(tool_use) => {
                    // Move focus back to Input when tool use is cancelled.
                    if let Some(idx) = self.messages.locate_tool_message(&tool_use.id)
                        && self.messages.selected_idx() == Some(idx)
                    {
                        self.update_focus(Focus::Input);
                        self.messages.blur();
                    }
                    // Await the next user message to avoid the LLM reacting without further user
                    // instructions
                    self.pending_chats.push(code_combo::Block::ToolResult {
                        tool_use_id: tool_use.id.clone(),
                        is_error: Some(true),
                        content: code_combo::Content::Text("User cancelled".to_string()),
                    });
                }
                ToolAction::ApplyTextEdit {
                    id,
                    name,
                    edit,
                    context_radius,
                    hunk_idx,
                    is_rejecting,
                } => {
                    if *is_rejecting {
                        reject_text_edit(
                            id.to_owned(),
                            edit.to_owned(),
                            *context_radius,
                            *hunk_idx,
                        );
                    } else {
                        tokio::task::spawn(task_apply_text_edit(
                            self.agent.clone(),
                            id.to_owned(),
                            name.to_owned(),
                            edit.to_owned(),
                            *context_radius,
                            *hunk_idx,
                        ));
                    }
                }
            },
            _ => (),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let vertical = Layout::vertical([Min(0), Length(1), Length(1), Length(1)]);
        let [area_messages, divider, area_input, bottom] = vertical.areas(area);

        self.messages.draw(frame, area_messages)?;

        let block = Block::new()
            .borders(Borders::BOTTOM)
            .border_set(border::THICK);
        frame.render_widget(self.block_bottom_with_shortcuts_desc(block), divider);

        let mut block = Block::new().borders(Borders::BOTTOM);
        block = if !matches!(self.focus.read(), Focus::Messages) {
            block.border_set(border::THICK).border_style(Style::reset())
        } else {
            block
                .border_set(border::PLAIN)
                .border_style(Style::default().dark_gray())
        };
        frame.render_widget(
            block
                .title_bottom(Line::from(""))
                .title_bottom(self.widget_state_indicator()),
            bottom,
        );
        self.input.draw(frame, area_input)
    }
}

async fn task_chat(mut agent: Agent, content: ChatContent) {
    let tx = global::event_tx();

    let msg = ChatMessage::user(content);
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();

    let msg = agent.chat(msg).await;

    let mut to_execute: Vec<code_combo::ToolUse> = vec![];
    tx.send(
        AnswerEvent::Bot(match msg.content {
            ChatContent::Text(text) => {
                vec![BotMessage::Plain(text)]
            }
            ChatContent::Multiple(blocks) => {
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
                        code_combo::Block::ToolUse(tool_use) => BotMessage::ToolUse(tool_use),
                        _ => unreachable!("unknown content type: {:?}", m),
                    })
                    .collect()
            }
        })
        .into(),
    )
    .unwrap();

    if !to_execute.is_empty() {
        debug!("run {} executions parallelly", to_execute.len());
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
}

async fn task_tool_use(mut agent: Agent, tool_use: ToolUse) {
    let tx = global::event_tx();
    let code_combo::ToolUse { id, name, input } = tool_use;
    // It will be executed if permission check pass
    let rv = agent
        .execute(&id, &name, code_combo::Input::Starter(input))
        .await;
    let is_error = matches!(rv, Output::Failure(_));
    match rv {
        Output::AskPermission => tx.send(AskEvent::ToolUsePermission(id).into()).unwrap(),
        Output::Success(output) | Output::Failure(output) => {
            tx.send(
                AnswerEvent::ToolResult {
                    id,
                    is_error,
                    output,
                }
                .into(),
            )
            .unwrap();
        }
        Output::TextEdit(edit) => {
            tx.send(AskEvent::TextEdit { id, edit }.into()).unwrap();
        }
        Output::Denied => (),
    }
}

fn reject_text_edit(id: String, edit: TextEdit, context_radius: usize, hunk_idx: usize) {
    let tx = global::event_tx();

    let new_edit = edit.reject_hunk(context_radius, hunk_idx);
    if let Some(edit) = new_edit {
        // Notify components that text edits have been updated and need confirmation again
        tx.send(AskEvent::TextEdit { id, edit }.into()).unwrap();
    } else {
        let event = if edit.changed() {
            AnswerEvent::ToolResult {
                id,
                is_error: false,
                output: "user rejects some changes".into(),
            }
        } else {
            AnswerEvent::ToolResult {
                id,
                is_error: true,
                output: "user rejects all changes".into(),
            }
        };
        tx.send(event.into()).unwrap();
    }
}

async fn task_apply_text_edit(
    mut agent: Agent,
    id: String,
    name: String,
    mut edit: TextEdit,
    context_radius: usize,
    hunk_idx: usize,
) {
    let tx = global::event_tx();

    let (applied, new_edit) = edit
        .apply_hunk(context_radius, hunk_idx)
        .expect("should apply successfully");

    let rv = agent
        .execute(&id, &name, code_combo::Input::AppliedTextEdit(applied))
        .await;
    let is_error = matches!(rv, Output::Failure(_));
    match rv {
        Output::Success(output) | Output::Failure(output) => {
            if is_error || new_edit.is_none() {
                // End the tool use if there's an error or no more text edits to apply
                tx.send(
                    AnswerEvent::ToolResult {
                        id,
                        is_error,
                        output,
                    }
                    .into(),
                )
                .unwrap();
            } else if let Some(edit) = new_edit {
                // Notify components that text edits have been updated and need confirmation again
                tx.send(AskEvent::TextEdit { id, edit }.into()).unwrap();
            }
        }
        _ => (),
    }
}
