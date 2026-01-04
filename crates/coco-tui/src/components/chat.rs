use anthropic::Block as AnthropicBlock;
use coco_macro::ComponentExt;
use code_combo::tools::Final;
use code_combo::{
    Agent, Block as ChatBlock, ChatResponse, Config, Content as ChatContent,
    Message as ChatMessage, Output, SessionEnv, StarterCommand, StarterError, StarterEvent,
    StopReason, TextEdit, ToolUse, discover_starters,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    prelude::*,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders},
};

use serde::{Deserialize, Serialize};
use std::time::Duration;
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    Action, AnswerEvent, AskEvent, BotMessage, CacheInvalidation, Combo, ComboAction, ComboEvent,
    CommandPaletteAction, Component, Event, Input, Message, Messages, NavigationKey,
    NavigationResult, Plain, SessionAction, ShortcutHints, ShortcutHintsPanel, Tool, ToolAction,
    TranscriptMessage,
};
use crate::{
    components::{CommandPalette, Content, Persistable},
    error::*,
    global::{self, State},
    session::{self, Session},
    widgets::{BRAILLE_EIGHT_DOUBLE, Throbber, ThrobberState},
};

#[derive(ComponentExt)]
#[component(type_id = "chat")]
pub struct Chat<'a> {
    state: State<Inner>,

    agent: Agent,

    command_palette: CommandPalette,
    input: Input<'a>,
    messages: Messages,
    transcript: Messages,
    view: ViewMode,
    indicator: ThrobberState,
    shortcut_hints: ShortcutHintsPanel,
    prev_focus: Option<Focus>,

    token_schedule_session_save: Option<CancellationToken>,
    cancellation_guard: CancellationGuard,
}

#[derive(Clone, Serialize, Deserialize)]
struct Inner {
    // Placeholder field for serialization
    system_prompt: String,
    messages: Vec<code_combo::Message>,

    state: ChatState,
    focus: Focus,
    #[serde(default)]
    auto_accept_edits: bool,
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
            system_prompt: String::new(),
            messages: vec![],
            state: ChatState::Ready,
            focus: Focus::Input,
            auto_accept_edits: false,
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

#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
enum ChatState {
    #[default]
    Ready,
    Procesing,
}

impl std::fmt::Display for ChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready => f.write_str("Ready"),
            Self::Procesing => f.write_str("Procesing"),
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
    ShortcutHints,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewMode {
    Chat,
    Transcript,
}

const CTRL_C_WINDOW: Duration = Duration::from_secs(2);
#[derive(Debug, Default)]
struct CancellationGuard {
    last_hit: State<Option<Instant>>,
    cancel_token: Option<CancellationToken>,
}

impl CancellationGuard {
    pub fn try_fire(&mut self) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_hit.get()
            && now.duration_since(last) <= CTRL_C_WINDOW
        {
            // fire
            self.cancel_token();
            self.reset();
            return true;
        }

        *self.last_hit.write() = Some(now);

        false
    }

    pub fn reset(&mut self) {
        *self.last_hit.write() = None;
    }

    pub fn on_trick(&mut self) {
        let Some(last) = self.last_hit.get() else {
            return;
        };
        if Instant::now().duration_since(last) > CTRL_C_WINDOW {
            self.reset();
        }
    }

    pub fn is_armed(&self) -> bool {
        self.last_hit.is_some()
    }

    pub fn token(&mut self) -> CancellationToken {
        if let Some(token) = &self.cancel_token {
            return token.clone();
        }
        let token = CancellationToken::new();
        self.cancel_token = Some(token.clone());
        token
    }

    pub fn start_token(&mut self) -> CancellationToken {
        self.cancel_token();
        self.reset();
        self.token()
    }

    fn cancel_token(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }
}

const AGENTS_MD_FILENAME: &str = "AGENTS.md";

impl Chat<'static> {
    pub fn new(config: Config) -> Self {
        let agent = Agent::new(config);

        Self {
            state: State::default(),
            agent,
            command_palette: CommandPalette::new(),
            input: Input::default(),
            messages: Messages::default(),
            transcript: Messages::default(),
            view: ViewMode::Chat,
            indicator: ThrobberState::default(),
            shortcut_hints: ShortcutHintsPanel::default(),
            prev_focus: None,
            token_schedule_session_save: None,
            cancellation_guard: CancellationGuard::default(),
        }
    }

    pub async fn setup(&mut self) {
        // read AGENTS.md file
        let workspace_path = global::workspace_dir().join(AGENTS_MD_FILENAME);
        let global_path = global::config().await.config_dir.join(AGENTS_MD_FILENAME);
        let mut custom_parts = Vec::new();
        for path in [global_path, workspace_path] {
            match tokio::fs::read_to_string(&path).await {
                Ok(system_prompt) => {
                    if !system_prompt.trim().is_empty() {
                        custom_parts.push(system_prompt);
                    }
                }
                Err(err) => {
                    warn!(?path, ?err, "failed to read file");
                }
            }
        }

        if custom_parts.is_empty() {
            self.agent
                .set_system_prompt(&Agent::build_system_prompt(None));
            self.state.write_untracked().system_prompt.clear();
        } else {
            let combined_custom = custom_parts.join("\n\n");
            let combined = Agent::build_system_prompt(Some(&combined_custom));
            self.agent.set_system_prompt(&combined);
            self.state.write_untracked().system_prompt = combined_custom;
        }
    }

    fn restore_session_by_metadata(&self, metadata: session::PersistentSessionMetadata) {
        let filename = metadata.filename();
        let session_dir = std::path::Path::new(".coco/sessions").to_path_buf();
        tokio::spawn(async move {
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
        });
    }

    fn schedule_save_task(&mut self, save_at: Instant) {
        // Cancel existing timer if any
        if let Some(token) = self.token_schedule_session_save.take() {
            token.cancel();
        }

        let token = CancellationToken::new();
        self.token_schedule_session_save = Some(token.clone());

        let mut state = self.state.get();
        if state.focus == Focus::ShortcutHints {
            state.focus = self.prev_focus.clone().unwrap_or_default();
        }
        if state.state == ChatState::Procesing {
            // Persist Ready to avoid restoring a stale processing state.
            state.state = ChatState::Ready;
        }
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
            ComboEvent::Discovering
            | ComboEvent::Executing { .. }
            | ComboEvent::Output { .. }
            | ComboEvent::RecordStart { .. }
            | ComboEvent::RecordOutput { .. }
            | ComboEvent::RecordEnd { .. }
            | ComboEvent::Prompt { .. } => {
                self.set_processing();
            }
            ComboEvent::Executed { starter, .. } => {
                if let Err(err) = starter.combo.as_ref() {
                    warn!(?err, "Failed to execute starter");
                }
                self.spawn_chat_with_history();
            }
            ComboEvent::Discovered { .. }
            | ComboEvent::NotFound { .. }
            | ComboEvent::Cancelled { .. } => {
                self.set_ready();
            }
            ComboEvent::ReplyToolError { message } => {
                self.messages
                    .push(Message::system(Plain::new(message.to_string()).into()));
                global::trigger_schedule_session_save();
            }
        }
    }

    fn update_focus(&mut self, new_focus: Focus) {
        let focus = self.state.focus.clone();
        if focus == new_focus {
            return;
        }
        debug!(?focus, ?new_focus, "update focus");
        if focus == Focus::Input {
            self.input.update(&Action::Blur);
        }
        if new_focus == Focus::Input {
            self.input.update(&Action::Focus);
        }
        if focus == Focus::ShortcutHints && new_focus != Focus::ShortcutHints {
            self.prev_focus = None;
        }
        self.state.write().focus = new_focus;
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

    fn spawn_chat_task(&mut self, content: ChatContent) {
        let cancel_token = self.cancellation_guard.start_token();
        tokio::task::spawn(task_chat(self.agent.clone(), content, cancel_token));
    }

    fn spawn_chat_with_history(&mut self) {
        let cancel_token = self.cancellation_guard.start_token();
        tokio::task::spawn(task_chat_with_history(self.agent.clone(), cancel_token));
    }

    fn spawn_combo_discover(&mut self) {
        let cancel_token = self.cancellation_guard.start_token();
        tokio::task::spawn(task_combo_discover(cancel_token));
    }

    fn spawn_combo_execute(&mut self, name: String, args: Vec<String>) {
        let cancel_token = self.cancellation_guard.start_token();
        let system_prompt = self.agent.system_prompt().to_string();
        let agent = self.agent.clone();
        tokio::task::spawn(task_combo_execute(
            name,
            args,
            system_prompt,
            cancel_token,
            agent,
        ));
    }

    fn spawn_tool_use(&mut self, tool_use: &ToolUse) {
        self.set_processing();
        let cancel_token = self.cancellation_guard.token();
        tokio::task::spawn(task_tool_use(
            self.agent.clone(),
            tool_use.to_owned(),
            cancel_token,
        ));
    }

    fn set_processing(&mut self) {
        if self.state.state != ChatState::Procesing {
            self.state.write().state = ChatState::Procesing;
        }
    }

    fn set_ready(&mut self) {
        if self.state.state != ChatState::Ready {
            // Avoid quitting the app while trying to cancel processing
            self.cancellation_guard.reset();
            self.state.write().state = ChatState::Ready;
        }
    }

    fn is_ready_for_exit(&self) -> bool {
        self.state.state == ChatState::Ready
    }

    fn handle_ctrl_c(&mut self) {
        if !self.cancellation_guard.try_fire() {
            return;
        }

        if self.is_ready_for_exit() {
            global::action_tx().send(Action::Quit).unwrap();
        }
    }

    fn submit_value(&mut self, value: String) {
        if value.is_empty() {
            debug!("submitting with empty value, skipping");
            return;
        }
        debug!(?value, "submiting");
        self.messages
            .push(Message::user(Plain::new(value.clone()).into()));
        let content = self.build_user_content(ChatContent::Text(value));
        self.spawn_chat_task(content);
    }

    fn on_submit(&mut self) {
        if self.state.state == ChatState::Ready {
            let value = self.input.clear();
            self.submit_value(value);
        } else {
            // TODO: Display an alert when input submission is not available
        }
    }

    fn toggle_auto_accept_edits(&mut self) {
        let enabled = {
            let mut state = self.state.write();
            state.auto_accept_edits = !state.auto_accept_edits;
            state.auto_accept_edits
        };
        self.agent.set_auto_accept_edits(enabled);
        global::trigger_schedule_session_save();
    }

    fn focus_for_shortcut_hints(&self) -> Focus {
        if self.state.focus == Focus::ShortcutHints {
            self.prev_focus.clone().unwrap_or(Focus::InputBlur)
        } else {
            self.state.focus.clone()
        }
    }

    fn open_shortcut_hints(&mut self) {
        if self.state.focus == Focus::ShortcutHints {
            return;
        }
        let prev_focus = self.state.focus.clone();
        self.prev_focus = Some(prev_focus);
        self.update_focus(Focus::ShortcutHints);
        global::signal_dirty();
    }

    fn close_shortcut_hints(&mut self) {
        if self.state.focus != Focus::ShortcutHints {
            self.prev_focus = None;
            return;
        }
        let prev_focus = self.prev_focus.take().unwrap_or(Focus::InputBlur);
        self.update_focus(prev_focus);
        global::signal_dirty();
    }

    fn input_shortcut_hints(&self) -> ShortcutHints {
        self.input.shortcut_hints()
    }

    fn input_blur_shortcut_hints(&self) -> ShortcutHints {
        let mut hints = ShortcutHints::default();
        hints.push_visible(&[("Focus", "CR")]);
        hints.push_visible(&[("Commands", "C-p")]);
        hints.push_visible(&[("Up", "k"), ("Down", "j")]);
        hints
    }

    fn chat_messages_shortcut_hints(&self) -> ShortcutHints {
        let mut hints = self.messages.shortcut_hints();
        if !self.messages.is_actionable() {
            hints.push_visible(&[("Back", "Esc")]);
        }
        hints.push_visible(&[("Up", "k"), ("Down", "j")]);
        hints.push_hidden(&[("Scroll Up", "C-y"), ("Down", "C-e")]);
        hints.push_hidden(&[("Scroll+ Up", "C-u"), ("Down", "C-d")]);
        hints
    }

    fn transcript_shortcut_hints(&self) -> ShortcutHints {
        let mut hints = self.transcript.shortcut_hints();
        hints.push_visible(&[("Back", "Esc")]);
        hints.push_visible(&[("Up", "k"), ("Down", "j")]);
        hints.push_hidden(&[("Scroll Up", "C-y"), ("Down", "C-e")]);
        hints.push_hidden(&[("Scroll+ Up", "C-u"), ("Down", "C-d")]);
        hints
    }

    fn current_shortcut_hints(&self) -> ShortcutHints {
        let focus = self.focus_for_shortcut_hints();
        match self.view {
            ViewMode::Transcript => self.transcript_shortcut_hints(),
            ViewMode::Chat => match focus {
                Focus::Input => self.input_shortcut_hints(),
                Focus::InputBlur => self.input_blur_shortcut_hints(),
                Focus::Messages => self.chat_messages_shortcut_hints(),
                Focus::CommandPalette | Focus::ShortcutHints => ShortcutHints::default(),
            },
        }
    }

    fn input_block_with_dynamic_titles<'a>(&'a self, mut block: Block<'a>) -> Block<'a> {
        block = block.title_top(Line::from(""));
        let focus = self.focus_for_shortcut_hints();
        let hints = match focus {
            Focus::Input => self.input_shortcut_hints(),
            Focus::InputBlur => self.input_blur_shortcut_hints(),
            Focus::Messages => self.chat_messages_shortcut_hints(),
            Focus::CommandPalette | Focus::ShortcutHints => ShortcutHints::default(),
        };
        block = match focus {
            Focus::CommandPalette | Focus::ShortcutHints => block,
            _ => self.shortcut_hints.decorate_block_top(block, &hints),
        };
        block = block
            .title_bottom(Line::from(""))
            .title_bottom(self.widget_state_indicator())
            .title_bottom(self.auto_accept_indicator());
        if let Some(line) = self.ctrl_c_reminder_line() {
            block = block.title_bottom(line);
        }
        block
    }

    fn draw_input(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let theme = global::theme();
        let mut block = Block::new().borders(Borders::TOP | Borders::BOTTOM);
        block = if matches!(self.state.focus, Focus::Input | Focus::InputBlur) {
            block
                .border_set(border::THICK)
                .border_style(theme.ui.block_border_active)
        } else {
            block
                .border_set(border::PLAIN)
                .border_style(theme.ui.block_border_inactive)
        };

        block = self.input_block_with_dynamic_titles(block);
        frame.render_widget(&block, area);
        self.input.draw(frame, block.inner(area))?;

        Ok(())
    }

    fn ctrl_c_reminder_line(&self) -> Option<Line<'static>> {
        if !self.cancellation_guard.is_armed() {
            return None;
        }
        let message = if self.is_ready_for_exit() {
            "Press Ctrl+C again to exit"
        } else {
            "Press Ctrl+C again to cancel"
        };
        let theme = global::theme();
        Some(Line::from(Span::styled(
            format!(" {message} "),
            theme.ui.status_warning,
        )))
    }

    fn widget_state_indicator(&self) -> Line<'_> {
        let theme = global::theme();
        let state = &self.state.state;
        match state {
            ChatState::Ready => {
                Line::from(Span::styled(format!(" {state} "), theme.ui.status_ready))
            }
            ChatState::Procesing => Line::from(vec![
                Span::raw(" "),
                Throbber::default()
                    .throbber_set(BRAILLE_EIGHT_DOUBLE)
                    .style(theme.ui.status_processing)
                    .to_symbol_span(&self.indicator),
                Span::styled(format!(" {state} "), theme.ui.status_processing),
            ]),
        }
    }

    fn auto_accept_indicator(&self) -> Line<'static> {
        let theme = global::theme();
        let (status, status_style) = if self.state.auto_accept_edits {
            ("on", theme.ui.auto_accept_on)
        } else {
            ("off", theme.ui.auto_accept_off)
        };
        Line::from(vec![
            Span::styled(" accept edits: ", theme.ui.shortcut_desc),
            Span::styled(status, status_style),
            Span::raw(" "),
            Span::styled("<S-Tab>", theme.ui.shortcut),
            Span::raw(" "),
        ])
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
            tx.send(
                AskEvent::TextEdit {
                    id,
                    edit,
                    auto_accept: false,
                }
                .into(),
            )
            .unwrap();
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

    fn open_transcript(&mut self) {
        self.update_focus(Focus::InputBlur);
        let agent_messages = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.agent.dump_messages())
        });

        self.transcript.clear();
        let iter = agent_messages
            .into_iter()
            .map(|message| Message::system(TranscriptMessage::new(message).into()));
        self.transcript.extend(iter);
        self.view = ViewMode::Transcript;
        global::signal_dirty();
    }

    fn close_transcript(&mut self) {
        self.view = ViewMode::Chat;
        self.update_focus(Focus::InputBlur);
        global::signal_dirty();
    }

    /// Handle the "New Session" command
    /// Optionally saves the current session before clearing
    fn new_session(&mut self) {
        // 1. Save current session if there are messages
        if !self.messages.is_empty() {
            // TODO: maybe don't save if being in processing state
            self.save_now();
        }

        let system_prompt = self.state.system_prompt.clone();
        let auto_accept_edits = self.state.auto_accept_edits;

        // 2. Clear messages
        self.messages.clear();

        // 3. Reset state
        *self.state.write() = Inner {
            focus: Focus::InputBlur,
            auto_accept_edits,
            system_prompt,
            ..Default::default()
        };
        self.cancellation_guard.reset();
        self.agent.set_auto_accept_edits(auto_accept_edits);

        // 4. Reset focus to Input from InputBlur to trigger the input component
        self.update_focus(Focus::Input);

        // 5. Cancel any pending save timer
        if let Some(token) = self.token_schedule_session_save.take() {
            token.cancel();
        }

        debug!("New session started");
        global::signal_dirty();
    }

    fn switch_theme(&mut self, theme: String) {
        let mut config = global::config_sync();
        if config.ui.theme == theme {
            return;
        }
        config.ui.theme = theme.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(global::set_config(config.clone()));
        });
        self.invalidate_cache(CacheInvalidation::Theme);

        global::signal_dirty();
    }
}

impl Persistable for Chat<'static> {
    fn save(&self) -> Session {
        // Chat persists via schedule_save_task, not through this method
        unreachable!("Chat has special way to do persisting")
    }

    fn load(session: Session) -> Result<Self> {
        let (mut state, session): (Inner, Session) = session::load_related(session)?;
        let mut inst = Self::new(global::config_sync());
        let auto_accept_edits = state.auto_accept_edits;

        if state.focus == Focus::ShortcutHints {
            state.focus = Focus::InputBlur;
        }

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(inst.agent.restore_messages(&state.messages));
            state.messages.clear();
        });
        let combined = Agent::build_system_prompt(Some(&state.system_prompt));
        inst.agent.set_system_prompt(&combined);
        inst.agent.set_auto_accept_edits(auto_accept_edits);

        inst.state = State::new(state);
        inst.messages = Messages::load(session)?;
        Ok(inst)
    }
}

impl Component for Chat<'static> {
    fn children(&'_ mut self) -> Box<dyn Iterator<Item = &'_ mut dyn Component> + '_> {
        let children: Vec<&mut dyn Component> = vec![
            &mut self.input,
            &mut self.messages,
            &mut self.transcript,
            &mut self.shortcut_hints,
        ];
        Box::new(children.into_iter())
    }

    fn on_tick(&mut self) {
        self.cancellation_guard.on_trick();

        if self.state.state == ChatState::Procesing {
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
                self.set_processing();
            }
            Event::Answer(AnswerEvent::Bot(msgs)) => {
                self.set_ready();
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
            Event::Answer(AnswerEvent::Cancelled) => {
                self.set_ready();
            }
            Event::Ask(AskEvent::ToolUsePermission(_)) => {
                if let Some(idx) = self.messages.on_tool_event(event) {
                    // Move focus to tool use message when permission is required
                    self.update_focus(Focus::Messages);
                    self.messages.focus(idx);
                }
                // Trigger session save after ask event
                global::trigger_schedule_session_save();
            }
            Event::Ask(AskEvent::TextEdit { auto_accept, .. }) => {
                if let Some(idx) = self.messages.on_tool_event(event)
                    && !*auto_accept
                {
                    // Move focus to tool use message when confirmation is required
                    self.update_focus(Focus::Messages);
                    self.messages.focus(idx);
                }
                // Trigger session save after ask event
                global::trigger_schedule_session_save();
            }
            Event::Answer(AnswerEvent::ToolResult {
                id,
                is_user_cancelled,
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
                let mut content: code_combo::Content = output.try_into().unwrap();
                if *is_user_cancelled {
                    content = content.user_cancel();
                }
                // TODO: Allow user to retry if tool use fails.
                let content = ChatContent::Multiple(vec![code_combo::Block::ToolResult {
                    tool_use_id: id.clone(),
                    is_error: Some(*is_error),
                    content,
                }]);
                let content = self.build_user_content(content);
                self.spawn_chat_task(content);
                // Trigger session save after tool result
                global::trigger_schedule_session_save();
            }
            Event::Answer(AnswerEvent::ToolOutput { .. }) => {
                let _ = self.messages.on_tool_event(event);
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

        if matches!(key.code, BackTab) {
            if self.view == ViewMode::Chat {
                self.toggle_auto_accept_edits();
            }
            return;
        }

        let focus = &self.state.focus;
        if matches!(
            key,
            KeyEvent {
                code: Char('c') | Char('C'),
                modifiers: KM::CONTROL,
                ..
            }
        ) {
            self.handle_ctrl_c();
            return;
        }
        if self.view == ViewMode::Transcript {
            match (key.modifiers, key.code) {
                (KM::NONE, Esc) => self.close_transcript(),
                (KM::NONE, Char('k')) => {
                    self.transcript.scroll_up(1);
                }
                (KM::NONE, Char('j')) => {
                    self.transcript.scroll_down(1);
                }
                (KM::CONTROL, Char('y')) => {
                    self.transcript.scroll_up(1);
                }
                (KM::CONTROL, Char('e')) => {
                    self.transcript.scroll_down(1);
                }
                (KM::CONTROL, Char('u')) => {
                    self.transcript.scroll_half_up();
                }
                (KM::CONTROL, Char('d')) => {
                    self.transcript.scroll_half_down();
                }
                _ => (),
            }
            return;
        }

        match (focus, key.modifiers, key.code) {
            // Focus switching
            (Input, KM::NONE, Esc) => self.update_focus(InputBlur),
            (InputBlur, KM::NONE, Enter) => self.update_focus(Input),
            (InputBlur, KM::CONTROL, Char('p')) => {
                self.command_palette.open(self.state.created_at);
                self.update_focus(CommandPalette);
            }
            (Messages, KM::NONE, Esc) if !self.messages.is_actionable() => {
                self.messages.blur();
                self.update_focus(Focus::InputBlur);
            }
            (InputBlur | Messages, KM::NONE, Char('?'))
                if self.current_shortcut_hints().has_hidden() =>
            {
                self.open_shortcut_hints();
            }
            (ShortcutHints, _, _) => {
                self.close_shortcut_hints();
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
                let _ = self.messages.handle_navigation(NavigationKey::Up);
            }
            (Messages, KM::NONE, Char('j')) => {
                if self.messages.handle_navigation(NavigationKey::Down)
                    == NavigationResult::Boundary
                {
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
            (CommandPalette, KM::NONE, Esc) => {
                if !self.command_palette.on_escape() {
                    self.update_focus(InputBlur);
                }
            }
            (CommandPalette, _, _) => self.command_palette.handle_key_event(key),

            (InputBlur, _, _) => {
                warn!(?key, ?focus, "unknown key event");
            }
        }
    }

    fn update(&mut self, action: &Action) {
        debug!(?action, "updating");

        match action {
            Action::Combo(combo_action) => match combo_action {
                ComboAction::Discover => {
                    self.spawn_combo_discover();
                }
                ComboAction::Execute { name, args } => {
                    let combo = Combo::new(name);
                    self.messages.push(Message::user(combo.into()));
                    self.spawn_combo_execute(name.clone(), args.clone());
                    debug!("Combo message pushed");
                }
            },
            Action::Tool(action) => match action {
                ToolAction::Grant(tool_use) => {
                    self.agent.grant_once(&tool_use.id, &tool_use.name);
                    self.spawn_tool_use(tool_use);
                }
                ToolAction::GrantSession(tool_use) => {
                    self.agent.grant_session(tool_use);
                    self.spawn_tool_use(tool_use);
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
                        self.set_processing();
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
            Action::CommandPalette(action) => match action {
                CommandPaletteAction::NewSession => {
                    self.update_focus(Focus::InputBlur);
                    self.new_session();
                }
                CommandPaletteAction::Transcript => {
                    self.update_focus(Focus::InputBlur);
                    self.open_transcript();
                }
                CommandPaletteAction::RestoreSession(metadata) => {
                    self.update_focus(Focus::InputBlur);
                    if !self.messages.is_empty() {
                        self.save_now();
                    }
                    self.restore_session_by_metadata(metadata.to_owned());
                }
                CommandPaletteAction::SwitchTheme(theme) => {
                    self.update_focus(Focus::InputBlur);
                    self.switch_theme(theme.to_owned());
                }
                CommandPaletteAction::Shell => {
                    self.update_focus(Focus::InputBlur);
                }
            },
            Action::SubmitPrompt(prompt) => {
                if self.state.state == ChatState::Ready {
                    self.submit_value(prompt.to_owned());
                } else {
                    warn!(
                        ?prompt,
                        state = %self.state.state,
                        "auto submit skipped while busy"
                    );
                }
            }
            _ => (),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        match self.view {
            ViewMode::Chat => self.draw_chat(frame, area)?,
            ViewMode::Transcript => self.draw_transcript(frame, area)?,
        }

        if self.state.focus == Focus::CommandPalette {
            // popup floating window
            self.command_palette.draw(frame, area)?;
        }

        if self.state.focus == Focus::ShortcutHints {
            let hints = self.current_shortcut_hints();
            self.shortcut_hints.set_hints(hints);
            self.shortcut_hints.draw(frame, area)?;
        }

        Ok(())
    }
}

impl Chat<'static> {
    fn draw_chat(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let theme = global::theme();
        frame.render_widget(Block::new().style(theme.ui.chat_bg), area);

        let vertical = Layout::vertical([Min(0), Length(3)]);
        let [area_messages, area_input] = vertical.areas(area);

        self.messages.draw(frame, area_messages)?;
        self.draw_input(frame, area_input)?;

        Ok(())
    }

    fn draw_transcript(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        use Constraint::{Length, Min};

        let theme = global::theme();
        frame.render_widget(Block::new().style(theme.ui.chat_bg), area);

        let vertical = Layout::vertical([Min(0), Length(1)]);
        let [area_messages, bottom] = vertical.areas(area);
        self.transcript.draw(frame, area_messages)?;

        let mut bottom_block = Block::new()
            .borders(Borders::BOTTOM)
            .border_set(border::THICK)
            .border_style(theme.ui.block_border_active);
        bottom_block = bottom_block
            .title_bottom(Line::from(""))
            .title_bottom(Line::from(" Transcript ").bold());
        let hints = self.transcript_shortcut_hints();
        bottom_block = self
            .shortcut_hints
            .decorate_block_bottom(bottom_block, &hints);
        if let Some(line) = self.ctrl_c_reminder_line() {
            bottom_block = bottom_block.title_bottom(line);
        }
        frame.render_widget(bottom_block, bottom);

        Ok(())
    }
}

async fn task_combo_discover(cancel_token: CancellationToken) {
    let tx = global::event_tx();
    let config = global::config().await;
    let combo_dir = config.combo_dir();
    let workspace_combo_dir = global::workspace_combo_dir();

    tx.send(ComboEvent::Discovering.into()).unwrap();
    let result = discover_starters(&[&workspace_combo_dir, &combo_dir], cancel_token).await;
    if result.cancelled {
        tx.send(ComboEvent::Cancelled { name: None }.into())
            .unwrap();
        return;
    }
    tx.send(
        ComboEvent::Discovered {
            starters: result.starters,
        }
        .into(),
    )
    .unwrap();
}

fn format_command_line(command: &str, args: &[String]) -> String {
    let command = resolve_command_display(command);
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_escape(&command));
    for arg in args {
        parts.push(shell_escape(arg));
    }
    parts.join(" ")
}

fn resolve_command_display(command: &str) -> String {
    let command_path = std::path::Path::new(command);
    let workspace_combo_dir = global::workspace_combo_dir();
    if let Ok(relative) = command_path.strip_prefix(&workspace_combo_dir) {
        let display_path = std::path::Path::new(".coco/combos").join(relative);
        return display_path.to_string_lossy().to_string();
    }
    command.to_string()
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.bytes().all(|byte| {
        matches!(byte, b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'_'
            | b'-'
            | b'.'
            | b'/'
            | b':'
            | b'@'
            | b'+'
            | b'='
            | b','
            | b'%')
    }) {
        return value.to_string();
    }
    let mut escaped = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            escaped.push_str("'\"'\"'");
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('\'');
    escaped
}

async fn task_combo_execute(
    name: String,
    args: Vec<String>,
    system_prompt: String,
    cancel_token: CancellationToken,
    mut agent: Agent,
) {
    let tx = global::event_tx();
    let config = global::config().await;
    let combo_dir = config.combo_dir();
    let workspace_combo_dir = global::workspace_combo_dir();

    tx.send(ComboEvent::Discovering.into()).unwrap();
    let result = discover_starters(&[&workspace_combo_dir, &combo_dir], cancel_token.clone()).await;
    if result.cancelled || cancel_token.is_cancelled() {
        tx.send(
            ComboEvent::Cancelled {
                name: Some(name.clone()),
            }
            .into(),
        )
        .unwrap();
        return;
    }

    let Some(starter) = result
        .starters
        .into_iter()
        .find(|starter| match &starter.combo {
            Ok(combo) => combo.metadata.name == name,
            Err(err) => {
                warn!(?starter.path, ?err, "Failed to load combo");
                false
            }
        })
    else {
        tx.send(ComboEvent::NotFound { name: name.clone() }.into())
            .unwrap();
        return;
    };

    let command_line = format_command_line(&starter.path, &args);

    // Skip the `ComboEvent::Discovered` event and advance directly to `ComboEvent::Executing`
    tx.send(
        ComboEvent::Executing {
            name: name.clone(),
            command_line,
        }
        .into(),
    )
    .unwrap();

    let session_env = SessionEnv::builder()
        .build()
        .expect("failed to build session");
    let mcp_envs = match code_combo::tools::prepare_mcp_envs().await {
        Ok(envs) => envs,
        Err(err) => {
            warn!(?err, "failed to prepare mcp envs for combo");
            Vec::new()
        }
    };
    let starter_path = starter.path.clone();
    let mut exit_code: Option<i32> = None;

    let mut execution = StarterCommand::new(&starter.path)
        .args(args)
        .envs(mcp_envs)
        .session_env(session_env)
        .execute();
    let mut cancelled = false;
    loop {
        tokio::select! {
            _ = cancel_token.cancelled(), if !cancelled => {
                cancelled = true;
                execution.cancel();
            }
            event = execution.next() => {
                let Some(event) = event else { break };
                match event {
                    StarterEvent::Output { chunk } => {
                        tx.send(
                            ComboEvent::Output {
                                name: name.clone(),
                                chunk,
                            }
                            .into(),
                        )
                        .unwrap();
                    }
                    StarterEvent::RecordOutput { tool_use_id, chunk } => {
                        tx.send(
                            ComboEvent::RecordOutput {
                                name: name.clone(),
                                tool_use_id,
                                chunk,
                            }
                            .into(),
                        )
                        .unwrap();
                    }
                    StarterEvent::RecordStart { tool_use } => {
                        agent
                            .append_message(build_tool_use_message(&tool_use))
                            .await;
                        tx.send(
                            ComboEvent::RecordStart {
                                name: name.clone(),
                                tool_use,
                            }
                            .into(),
                        )
                        .unwrap();
                    }
                    StarterEvent::RecordEnd {
                        tool_use_id,
                        is_error,
                        output,
                    } => {
                        agent
                            .append_message(build_tool_result_message(
                                &tool_use_id,
                                is_error,
                                &output,
                            ))
                            .await;
                        tx.send(
                            ComboEvent::RecordEnd {
                                name: name.clone(),
                                tool_use_id,
                                is_error,
                                output,
                            }
                            .into(),
                        )
                        .unwrap();
                    }
                    StarterEvent::Prompt { prompt } => {
                        agent
                            .append_message(build_prompt_message(&prompt))
                            .await;
                        tx.send(
                            ComboEvent::Prompt {
                                name: name.clone(),
                                prompt,
                            }
                            .into(),
                        )
                        .unwrap();
                    }
                    StarterEvent::PromptRequest {
                        prompt,
                        schemas,
                        responder,
                    } => {
                        agent
                            .append_message(build_prompt_message(&prompt))
                            .await;
                        tx.send(
                            ComboEvent::Prompt {
                                name: name.clone(),
                                prompt: prompt.clone(),
                            }
                            .into(),
                        )
                        .unwrap();

                        let response = if cancel_token.is_cancelled() {
                            Err("prompt reply cancelled".to_string())
                        } else {
                            agent
                                .reply_prompt(&system_prompt, schemas)
                                .await
                                .map(|reply| reply.response)
                                .map_err(|err| err.to_string())
                        };
                        if let Err(err) = &response {
                            tx.send(
                                ComboEvent::ReplyToolError {
                                    message: err.clone(),
                                }
                                .into(),
                            )
                            .ok();
                        }
                        if let Err(err) = responder.send(response) {
                            tx.send(
                                ComboEvent::ReplyToolError { message: err }.into(),
                            )
                            .ok();
                        }
                    }
                    StarterEvent::Finished { exit_code: code } => {
                        exit_code = code;
                    }
                    _ => (),
                }
            }
        }
    }

    let starter = match execution.wait().await {
        Ok(starter) => starter,
        Err(err) => {
            warn!(?err, "starter join error");
            let error_message = format!("Combo execution failed: starter join error: {err}");
            agent
                .append_message(ChatMessage::user(ChatContent::Text(error_message)))
                .await;
            let starter = code_combo::Starter {
                path: starter_path,
                combo: Err(StarterError::Invalid {
                    reason: format!("starter join error: {err}"),
                }),
            };
            tx.send(
                ComboEvent::Executed {
                    name: name.clone(),
                    starter,
                    exit_code,
                }
                .into(),
            )
            .unwrap();
            return;
        }
    };

    if cancel_token.is_cancelled() || matches!(&starter.combo, Err(StarterError::Cancelled)) {
        tx.send(
            ComboEvent::Cancelled {
                name: Some(name.clone()),
            }
            .into(),
        )
        .unwrap();
        return;
    }

    if let Err(err) = starter.combo.as_ref() {
        let error_message = format!("Combo execution failed: {err}");
        agent
            .append_message(ChatMessage::user(ChatContent::Text(error_message)))
            .await;
    } else if let Some(code) = exit_code
        && code != 0
    {
        let error_message = format!("Combo execution failed: exit code {code}");
        agent
            .append_message(ChatMessage::user(ChatContent::Text(error_message)))
            .await;
    }

    tx.send(
        ComboEvent::Executed {
            name: name.clone(),
            starter,
            exit_code,
        }
        .into(),
    )
    .unwrap();
}

fn build_tool_use_message(tool_use: &ToolUse) -> ChatMessage {
    ChatMessage::assistant(ChatContent::Multiple(vec![AnthropicBlock::tool_use(
        &tool_use.id,
        &tool_use.name,
        tool_use.input.clone(),
    )]))
}

fn build_tool_result_message(tool_use_id: &str, is_error: bool, output: &Final) -> ChatMessage {
    ChatMessage::user(ChatContent::Multiple(vec![AnthropicBlock::tool_result(
        tool_use_id,
        Some(is_error),
        final_to_content(output),
    )]))
}

fn build_prompt_message(prompt: &str) -> ChatMessage {
    ChatMessage::user(ChatContent::Text(prompt.to_string()))
}

fn final_to_content(output: &Final) -> ChatContent {
    let text = match output {
        Final::Json(value) => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        Final::Message(message) => message.clone(),
    };
    ChatContent::Text(text)
}

async fn task_chat(mut agent: Agent, content: ChatContent, cancel_token: CancellationToken) {
    let tx = global::event_tx();

    if cancel_token.is_cancelled() {
        return;
    }
    let msg = ChatMessage::user(content);
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();

    let chat_resp = tokio::select! {
        _ = cancel_token.cancelled() => {
            tx.send(AnswerEvent::Cancelled.into()).ok();
            return;
        }
        chat_resp = agent.chat(msg) => chat_resp.expect("failed to chat with LLM"),
    };

    handle_chat_response(agent, cancel_token, chat_resp).await;
}

async fn task_chat_with_history(mut agent: Agent, cancel_token: CancellationToken) {
    let tx = global::event_tx();

    if cancel_token.is_cancelled() {
        return;
    }
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();

    let chat_resp = tokio::select! {
        _ = cancel_token.cancelled() => {
            tx.send(AnswerEvent::Cancelled.into()).ok();
            return;
        }
        chat_resp = agent.chat_with_history() => chat_resp.expect("failed to chat with LLM"),
    };

    handle_chat_response(agent, cancel_token, chat_resp).await;
}

async fn handle_chat_response(
    agent: Agent,
    cancel_token: CancellationToken,
    chat_resp: ChatResponse,
) {
    let tx = global::event_tx();
    let mut to_execute: Vec<code_combo::ToolUse> = vec![];
    let mut bot_messages = match chat_resp.message.content {
        ChatContent::Text(text) => vec![BotMessage::Plain(text)],
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
        if cancel_token.is_cancelled() {
            return;
        }
        // Parallel execution
        let handles = to_execute
            .into_iter()
            .map(|tool_use| {
                let agent = agent.clone();
                let cancel_token = cancel_token.clone();
                tokio::task::spawn(task_tool_use(agent, tool_use, cancel_token))
            })
            .collect::<Vec<_>>();
        tokio::select! {
            _ = cancel_token.cancelled() => {}
            _ = futures::future::join_all(handles) => {}
        }
    }
}

async fn task_tool_use(mut agent: Agent, tool_use: ToolUse, cancel_token: CancellationToken) {
    let tx = global::event_tx();
    let code_combo::ToolUse { id, name, input } = tool_use.clone();
    let auto_accept = agent.auto_accept_edits();
    let _ = agent
        .execute_with_output(
            &id,
            &name,
            code_combo::Input::Starter(input),
            cancel_token.clone(),
            |out| match out {
                Output::ToolOutput(chunk) => {
                    tx.send(
                        AnswerEvent::ToolOutput {
                            id: id.clone(),
                            chunk,
                        }
                        .into(),
                    )
                    .unwrap();
                }
                Output::AskPermission => {
                    tx.send(AskEvent::ToolUsePermission(id.clone()).into())
                        .unwrap();
                }
                Output::TextEdit(edit) => {
                    tx.send(
                        AskEvent::TextEdit {
                            id: id.clone(),
                            edit,
                            auto_accept,
                        }
                        .into(),
                    )
                    .unwrap();
                }
                Output::Success(output) => {
                    tx.send(
                        AnswerEvent::ToolResult {
                            id: id.clone(),
                            is_error: false,
                            is_user_cancelled: cancel_token.is_cancelled(),
                            output,
                        }
                        .into(),
                    )
                    .unwrap();
                }
                Output::Failure(output) => {
                    tx.send(
                        AnswerEvent::ToolResult {
                            id: id.clone(),
                            is_error: true,
                            is_user_cancelled: cancel_token.is_cancelled(),
                            output,
                        }
                        .into(),
                    )
                    .unwrap();
                }
                Output::Denied => (),
            },
        )
        .await;
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
                        is_user_cancelled: false,
                        output,
                    }
                    .into(),
                )
                .unwrap();
            } else if let Some(edit) = new_edit {
                // Notify components that text edits have been updated and need confirmation again
                tx.send(
                    AskEvent::TextEdit {
                        id,
                        edit,
                        auto_accept: false,
                    }
                    .into(),
                )
                .unwrap();
            }
        }
        _ => (),
    }
}
