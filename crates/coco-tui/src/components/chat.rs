use coco_macro::ComponentExt;
use code_combo::{
    Agent, Block as ChatBlock, Config, Content as ChatContent, Message as ChatMessage, Output,
    StopReason, TextEdit, ToolUse,
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

use serde::{Deserialize, Serialize};
use throbber_widgets_tui::{Throbber, ThrobberState};
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    Action, AnswerEvent, AskEvent, BotMessage, Combo, ComboAction, ComboEvent, Component, Content,
    Event, Input, Message, Messages, Plain, SessionAction, Tool, ToolAction, shortcuts_desc,
};
use crate::{
    components::{Command, CommandPalette, Persistable},
    error::*,
    global::{self, State},
    session::{self, Session},
};

#[derive(ComponentExt)]
#[component(type_id = "chat")]
pub struct Chat<'a> {
    state: State<Inner>,

    agent: Agent,

    command_palette: CommandPalette,
    input: Input<'a>,
    messages: Messages,
    indicator: ThrobberState,

    token_schedule_session_save: Option<CancellationToken>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Inner {
    // Placeholder field for serialization
    messages: Vec<code_combo::Message>,

    state: ChatState,
    focus: Focus,
    pending_chats: Vec<ChatBlock>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: time::OffsetDateTime,
    name: String,
}

impl Default for Inner {
    fn default() -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self {
            messages: vec![],
            state: ChatState::Ready,
            focus: Focus::Input,
            pending_chats: Vec::new(),
            created_at: now,
            updated_at: now,
            name: format!(
                "Session {}",
                now.format(&time::format_description::well_known::Rfc3339)
                    .expect("failed to format current time")
            ),
        }
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
enum Focus {
    #[default]
    Input,
    InputBlur,
    Messages,
    CommandPalette,
}

const COMMAND_NEW_SESSION: &str = "New Session";

impl Chat<'static> {
    pub fn new(config: Config) -> Self {
        Self {
            state: State::default(),
            agent: Agent::new(config),
            command_palette: CommandPalette::new(&[
                Command {
                    name: COMMAND_NEW_SESSION.to_string(),
                    shortcut: Some("<C-n>".to_string()),
                },
                // TODO: Switch Session
            ]),
            input: Input::default(),
            messages: Messages::default(),
            indicator: ThrobberState::default(),
            token_schedule_session_save: None,
        }
    }

    fn schedule_save_task(&mut self, save_at: Instant) {
        // Cancel existing timer if any
        if let Some(token) = self.token_schedule_session_save.take() {
            token.cancel();
        }

        let token = CancellationToken::new();
        self.token_schedule_session_save = Some(token.clone());

        let mut state = self.state.get();
        let messages = self.messages.save();
        let agent = self.agent.clone();

        tokio::spawn(async move {
            // Take a snapshot immediately to avoid persisting later dirty state
            state.messages = agent.dump_messages().await;

            let session_dir = std::path::Path::new(".coco/sessions").to_path_buf();
            if let Err(e) = tokio::fs::create_dir_all(&session_dir).await {
                warn!(?e, "failed to create session directory");
                return;
            }

            let now = time::OffsetDateTime::now_utc();
            let persistent_session = crate::session::PersistentSession {
                name: state.name.clone(),
                inner: session::save_related(&state, messages),
                created_at: state.created_at,
                updated_at: now,
            };

            tokio::select! {
                _ = token.cancelled() => (),
                _ = tokio::time::sleep_until(save_at) => {
                    if let Err(e) = crate::session::save_session(&session_dir, persistent_session).await {
                        warn!(?e, "failed to save session");
                    } else {
                        debug!("Session saved successfully");
                    }
                }
            }
        });
    }

    fn save_at(&mut self, save_at: Instant) {
        self.schedule_save_task(save_at);
    }

    fn save_now(&mut self) {
        self.schedule_save_task(Instant::now());
    }

    fn restore_last_session(&mut self) {
        let session_dir = std::path::Path::new(".coco/sessions").to_path_buf();

        tokio::spawn(async move {
            match crate::session::list_session(&session_dir).await {
                Ok(mut sessions) => {
                    if sessions.is_empty() {
                        debug!("No sessions to restore");
                        return;
                    }

                    // Sort by updated_at descending to get the most recent session
                    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

                    let last_session = &sessions[0];
                    let filename = last_session.filename();

                    match crate::session::load_session(&session_dir, &filename).await {
                        Ok(persistent_session) => {
                            global::action_tx()
                                .send(Action::restore_session(persistent_session.inner))
                                .unwrap();
                            debug!(name = %persistent_session.name, "Session restore requested");
                        }
                        Err(e) => {
                            warn!(?e, "failed to load session");
                        }
                    }
                }
                Err(e) => {
                    warn!(?e, "failed to list sessions");
                }
            }
        });
    }

    fn handle_combo_event(&mut self, event: &ComboEvent) {
        debug!(?event, "receive combo event");
        match event {
            ComboEvent::Discovering => {
                self.state.write().state = ChatState::ComboDiscovering;
            }
            ComboEvent::Executing { .. } | ComboEvent::Output { .. } => {
                self.state.write().state = ChatState::Procesing;
            }
            ComboEvent::Executed { starter, .. } => {
                let combo = starter.combo.as_ref().unwrap();
                let content = self.build_user_content(ChatContent::Text(combo.to_markdown()));
                tokio::task::spawn(task_chat(self.agent.clone(), content));
            }
            ComboEvent::Discovered { .. } | ComboEvent::NotFound { .. } => {
                self.state.write().state = ChatState::Ready;
            }
        }
    }

    fn update_focus(&mut self, new_focus: Focus) {
        let focus = &self.state.focus;
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
        self.state.write().focus = new_focus
    }

    /// Combines pending ToolResults (e.g., User Cancelled) with user instructions.
    ///
    /// This ensures the LLM doesn't react to tool results without explicit user instructions.
    /// Tool results are queued and combined with the next user message to provide context.
    fn build_user_content(&mut self, content: ChatContent) -> ChatContent {
        if self.state.pending_chats.is_empty() {
            content
        } else {
            let mut blocks = std::mem::take(&mut self.state.write().pending_chats);
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
        if matches!(self.state.state, ChatState::Ready) {
            let value = self.input.clear();
            debug!(?value, "submiting");
            self.messages
                .push(Message::user(Plain::new(value.clone()).into()));
            let content = self.build_user_content(ChatContent::Text(value));
            tokio::task::spawn(task_chat(self.agent.clone(), content));
        } else {
            // TODO: Display an alert when input submission is not available
        }
    }

    fn block_bottom_with_shortcuts_desc<'a>(&self, mut block: Block<'a>) -> Block<'a> {
        block = block.title_bottom(Line::from(""));
        match self.state.focus {
            Focus::Input => block
                .title_bottom(shortcuts_desc(&[("Blur", "Esc")]))
                .title_bottom(shortcuts_desc(&[("Submit", "CR")])),
            Focus::InputBlur => block
                .title_bottom(shortcuts_desc(&[("Focus", "CR")]))
                .title_bottom(shortcuts_desc(&[("Commands", "C-p")]))
                .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")])),
            Focus::Messages => {
                block = self.messages.block_with_shortcuts_desc(block);
                block
                    .title_bottom(shortcuts_desc(&[("Up", "k"), ("Down", "j")]))
                    .title_bottom(shortcuts_desc(&[("Scroll Up", "C-y"), ("Down", "C-e")]))
                    .title_bottom(shortcuts_desc(&[("Scroll+ Up", "C-u"), ("Down", "C-d")]))
            }
            Focus::CommandPalette => block,
        }
    }

    fn widget_state_indicator(&self) -> Line<'_> {
        let state = &self.state.state;
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

    fn reject_text_edit(
        &mut self,
        id: String,
        edit: TextEdit,
        context_radius: usize,
        hunk_idx: usize,
    ) {
        let tx = global::event_tx();

        let new_edit = edit.reject_hunk(context_radius, hunk_idx);
        if let Some(edit) = new_edit {
            // Notify components that text edits have been updated and need confirmation again
            tx.send(AskEvent::TextEdit { id, edit }.into()).unwrap();
        } else {
            // Move focus back to Input when the tool interaction ends.
            if let Some(idx) = self.messages.locate_tool_message(&id)
                && self.messages.selected_idx() == Some(idx)
            {
                self.update_focus(Focus::Input);
                self.messages.blur();
            }
            // Await the next user message to avoid the LLM reacting without further user
            // instructions
            self.state
                .write()
                .pending_chats
                .push(code_combo::Block::ToolResult {
                    tool_use_id: id,
                    is_error: Some(!edit.changed()),
                    content: if edit.changed() {
                        "User rejects some changes"
                    } else {
                        "User rejects all changes"
                    }
                    .into(),
                });
        }
    }

    /// Handle the "New Session" command
    /// Optionally saves the current session before clearing
    fn new_session(&mut self) {
        // 1. Save current session if there are messages
        if !self.messages.is_empty() {
            // TODO: maybe don't save if being in processing state
            self.save_now();
        }

        // 2. Clear messages
        self.messages.clear();

        // 3. Reset state
        *self.state.write() = Inner {
            focus: Focus::InputBlur,
            ..Default::default()
        };

        // 4. Reset focus to Input from InputBlur to trigger the input component
        self.update_focus(Focus::Input);

        // 5. Cancel any pending save timer
        if let Some(token) = self.token_schedule_session_save.take() {
            token.cancel();
        }

        debug!("New session started");
        global::signal_dirty();
    }
}

impl Persistable for Chat<'static> {
    fn save(&self) -> Session {
        // Save LLM messages
        let agent_messages = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.agent.dump_messages())
        });
        let mut state = self.state.get();
        state.messages = agent_messages;

        session::save_related(&state, self.messages.save())
    }

    fn load(session: Session) -> Result<Self> {
        let (mut state, session): (Inner, Session) = session::load_related(session)?;
        let mut inst = Self::new(global::config_sync());

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(inst.agent.restore_messages(&state.messages));
            state.messages.clear();
        });
        inst.state = State::new(state);
        inst.messages = Messages::load(session)?;
        Ok(inst)
    }
}

impl Component for Chat<'static> {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        let children: Vec<&mut dyn Component> = vec![&mut self.input, &mut self.messages];
        Box::new(children.into_iter())
    }

    fn on_tick(&mut self) {
        if matches!(
            self.state.state,
            ChatState::Procesing | ChatState::ComboDiscovering
        ) {
            self.indicator.calc_next();
            global::signal_dirty();
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
                self.state.write().state = ChatState::Procesing;
            }
            Event::Answer(AnswerEvent::Bot(msgs)) => {
                self.state.write().state = ChatState::Ready;
                self.messages
                    .extend(msgs.iter().cloned().map(|msg| match msg {
                        BotMessage::Plain(text) => Message::bot(Plain::new(text).into()),
                        BotMessage::ToolUse(tool_use) => {
                            Message::bot(Tool::new(tool_use.to_owned()).into())
                        }
                        BotMessage::System(message) => Message::system(Plain::new(message).into()),
                    }));
                // Trigger session save after receiving bot response
                global::trigger_schedule_session_save();
            }
            Event::Ask(AskEvent::ToolUsePermission(_) | AskEvent::TextEdit { .. }) => {
                if let Some(idx) = self.messages.on_tool_event(event) {
                    // Move focus to tool use message when permission is required
                    self.update_focus(Focus::Messages);
                    self.messages.focus(idx);
                }
                // Trigger session save after ask event
                global::trigger_schedule_session_save();
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
                // Trigger session save after tool result
                global::trigger_schedule_session_save();
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

        let focus = &self.state.focus;
        match (focus, key.modifiers, key.code) {
            // Focus switching
            (Input, KM::NONE, Esc) => self.update_focus(InputBlur),
            (InputBlur, KM::NONE, Enter) => self.update_focus(Input),
            (InputBlur, KM::CONTROL, Char('p')) => self.update_focus(CommandPalette),
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
            (CommandPalette, KM::NONE, Esc) => self.update_focus(InputBlur),
            (CommandPalette, _, _) => self.command_palette.handle_key_event(key),

            (InputBlur, _, _) => {
                warn!(?key, ?focus, "unknown key event");
            }
        }
    }

    fn update(&mut self, action: &Action) {
        debug!(?action, "updating");

        match action {
            Action::Combo(ComboAction::Execute { name }) => {
                let combo = Combo::new(name);
                self.messages.push(Message::user(combo.into()));
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
                    self.state
                        .write()
                        .pending_chats
                        .push(code_combo::Block::ToolResult {
                            tool_use_id: tool_use.id.clone(),
                            is_error: Some(true),
                            content: code_combo::Content::Text("User cancelled".to_string()),
                        });
                    // Trigger session save after tool cancellation
                    global::trigger_schedule_session_save();
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
                        self.reject_text_edit(
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
                    // Trigger session save after text edit operation
                    global::trigger_schedule_session_save();
                }
            },
            Action::Session(SessionAction::ScheduleSave { save_at }) => {
                // Schedule a debounced save
                self.state.write_untracked().updated_at = OffsetDateTime::now_utc();
                self.save_at(save_at.to_owned());
            }
            Action::Session(SessionAction::RestoreLastSession) => {
                self.restore_last_session();
            }
            Action::Session(SessionAction::RestoreSession(session)) => {
                match Self::load(session.clone()) {
                    Ok(restored) => {
                        *self = restored;
                        debug!(name = %self.state.name, "Session restored");
                        global::signal_dirty();
                    }
                    Err(e) => {
                        warn!(?e, "failed to restore session");
                    }
                }
            }
            Action::Command(name) => {
                // Close command palette
                self.update_focus(Focus::InputBlur);

                // Handle command
                match name.as_str() {
                    COMMAND_NEW_SESSION => {
                        self.new_session();
                    }
                    unknown => {
                        warn!(?unknown, "unknown command");
                    }
                }
            }
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
        block = if !matches!(self.state.focus, Focus::Messages) {
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
        self.input.draw(frame, area_input)?;

        if self.state.focus == Focus::CommandPalette {
            // popup floating window
            self.command_palette.draw(frame, area)?;
        }

        Ok(())
    }
}

async fn task_chat(mut agent: Agent, content: ChatContent) {
    let tx = global::event_tx();

    let msg = ChatMessage::user(content);
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();

    let chat_resp = agent.chat(msg).await.expect("failed to chat with LLM");

    let mut to_execute: Vec<code_combo::ToolUse> = vec![];
    let mut bot_messages = match chat_resp.message.content {
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
    };
    if let Some(reason) = chat_resp.stop_reason {
        let mut stop_executing = false;
        match reason {
            StopReason::MaxTokens => {
                bot_messages.push(BotMessage::System(
                    "Maximum token limit reached".to_string(),
                ));
                stop_executing = true;
            }
            StopReason::Refusal => {
                bot_messages.push(BotMessage::System(
                    "Response refused due to policy restrictions".to_string(),
                ));
                stop_executing = true;
            }
            _ => (),
        }

        if stop_executing {
            warn!(
                executions_cnt = to_execute.len(),
                ?reason,
                "Stop reason indicates unsafe execution, cancelling tool executions"
            );
            to_execute.clear();
        }
    }

    tx.send(AnswerEvent::Bot(bot_messages).into()).unwrap();

    if !to_execute.is_empty() {
        debug!(
            executions_cnt = to_execute.len(),
            "run executions parallelly"
        );
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
