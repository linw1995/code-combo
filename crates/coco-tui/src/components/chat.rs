use coco_macro::ComponentExt;
use code_combo::tools::{
    BASH_TOOL_NAME, BashInput, BashOutput, ComboEvent as ComboToolEvent, ComboInfo,
    ComboStreamKind as ComboToolStreamKind, Final, RUN_COMBO_TOOL_NAME, RUN_TASK_TOOL_NAME,
    RunComboInput, RunComboOutput, RunTaskInput, SubagentEvent,
};
use code_combo::{
    Agent, Block as ChatBlock, ChatResponse, ChatStreamUpdate, ComboRunEvent, ComboRunResult,
    ComboStreamKind as ComboRunStreamKind, Config, Content as ChatContent, Message as ChatMessage,
    Output, RetryAttempt, RetryNotifier, RetryUpdate, RuntimeOverrides, Starter, StopReason,
    TextEdit, ToolUse, UsageStats, discover_starters, load_runtime_overrides,
    save_runtime_overrides,
};

/// Holds information about a collected tool result from concurrent execution.
#[derive(Debug, Clone)]
struct CollectedToolResult {
    id: String,
    is_error: bool,
    content: code_combo::Content,
}
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    prelude::*,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use super::{
    Action, AnswerEvent, AskEvent, BotMessage, BotStreamKind, CacheInvalidation, Combo,
    ComboAction, ComboEvent, CommandPaletteAction, Component, Event, Input, Message, Messages,
    NavigationKey, NavigationResult, Plain, SessionAction, ShortcutHints, ShortcutHintsPanel,
    Thinking, Tool, ToolAction, TranscriptLinkKind, TranscriptLinkTarget, TranscriptMessage,
};
use crate::{
    components::{CommandPalette, Content, Persistable},
    error::*,
    global::{self, State},
    notifications,
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
    combo_thinking_active: bool,
    combo_tool_messages: HashSet<String>,
    manual_combo_runs: HashSet<String>,
    manual_combo_tool_uses: HashSet<String>,
    manual_combo_commands: HashMap<String, String>,
    manual_combo_prompted: HashSet<String>,
    pending_combo_tool_events: HashMap<String, Vec<ComboEvent>>,
    last_usage: Option<UsageStats>,
    retry_status: Option<RetryAttempt>,
    transcript_scopes: Vec<TranscriptScope>,
    terminal_focused: bool,

    /// Tool IDs we're expecting results from in concurrent execution.
    /// When empty, tool results are processed immediately.
    /// When non-empty, tool results are collected until all are received.
    pending_tool_ids: HashSet<String>,
    /// Collected tool results waiting to be sent together.
    collected_tool_results: Vec<CollectedToolResult>,

    token_schedule_session_save: Option<CancellationToken>,
    cancellation_guard: CancellationGuard,
}

#[derive(Clone, Serialize, Deserialize)]
struct ComboTranscript {
    id: String,
    name: String,
    messages: Vec<ChatMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SubagentTranscript {
    id: String,
    name: String,
    messages: Vec<ChatMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Inner {
    state: ChatState,
    focus: Focus,
    #[serde(default)]
    auto_accept_edits: bool,
    #[serde(default)]
    thinking_enabled: bool,
    #[serde(default)]
    model_override: Option<String>,
    pending_chats: Vec<ChatBlock>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: time::OffsetDateTime,
    name: String,

    // Placehold for session persistence
    #[serde(default)]
    system_prompt: Option<String>,
    messages: Vec<code_combo::Message>,
    #[serde(default)]
    combo_transcripts: Vec<ComboTranscript>,
    #[serde(default)]
    subagent_transcripts: Vec<SubagentTranscript>,
}

impl Default for Inner {
    fn default() -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self {
            system_prompt: None,
            messages: vec![],
            combo_transcripts: Vec::new(),
            subagent_transcripts: Vec::new(),
            state: ChatState::Ready,
            focus: Focus::Input,
            auto_accept_edits: false,
            thinking_enabled: false,
            model_override: None,
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

#[derive(Clone, Debug, PartialEq)]
enum TranscriptScope {
    Combo { id: String, name: String },
    Subagent { id: String, name: String },
}

const CTRL_C_WINDOW: Duration = Duration::from_secs(2);
const SESSION_SUMMARY_MAX_LEN: usize = 80;
const NOTIFY_TITLE: &str = "coco";
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

impl Chat<'static> {
    pub fn new(config: Config) -> Self {
        let mut agent = Agent::new(config);
        agent.set_ignore_workspace_scripts(global::ignore_workspace_scripts());
        agent.set_retry_notifier(Some(RetryNotifier::new(|update| {
            let tx = global::event_tx();
            tx.send(AnswerEvent::RetryUpdate { update }.into()).ok();
        })));

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
            combo_thinking_active: false,
            combo_tool_messages: HashSet::new(),
            manual_combo_runs: HashSet::new(),
            manual_combo_tool_uses: HashSet::new(),
            manual_combo_commands: HashMap::new(),
            manual_combo_prompted: HashSet::new(),
            pending_combo_tool_events: HashMap::new(),
            last_usage: None,
            retry_status: None,
            transcript_scopes: Vec::new(),
            terminal_focused: true,
            pending_tool_ids: HashSet::new(),
            collected_tool_results: Vec::new(),
            token_schedule_session_save: None,
            cancellation_guard: CancellationGuard::default(),
        }
    }

    pub async fn setup(&mut self) {
        let config = global::config().await;
        let config_dir = &config.config_dir;
        let workspace_dir = global::workspace_dir();

        self.agent
            .setup_system_prompt_async(config_dir, workspace_dir)
            .await;
    }

    pub fn apply_runtime_overrides(&mut self, overrides: RuntimeOverrides) {
        if let Some(auto_accept_edits) = overrides.auto_accept_edits {
            self.state.write_untracked().auto_accept_edits = auto_accept_edits;
            self.agent.set_auto_accept_edits(auto_accept_edits);
        }
        if let Some(thinking_enabled) = overrides.thinking_enabled {
            self.state.write_untracked().thinking_enabled = thinking_enabled;
            self.agent.set_thinking_enabled(thinking_enabled);
        }

        let model_override = overrides.model_override;
        self.state.write_untracked().model_override = model_override.clone();
        self.agent.set_model_override(model_override);
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

    fn schedule_save_task(&mut self, save_at: Instant, force_summary_refresh: bool) {
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
            state.system_prompt = Some(agent.system_prompt().to_string());

            let session_dir = std::path::Path::new(".coco/sessions").to_path_buf();
            if let Err(e) = tokio::fs::create_dir_all(&session_dir).await {
                warn!(?e, "failed to create session directory");
                return;
            }

            tokio::select! {
                _ = token.cancelled() => (),
                _ = tokio::time::sleep_until(save_at) => {
                    let summary = if force_summary_refresh {
                        generate_session_summary(
                            &state.messages,
                            state.model_override.clone(),
                            state.thinking_enabled,
                        )
                        .await
                    } else {
                        let metadata_filename =
                            session::PersistentSessionMetadata::metadata_filename_for_created_at(
                                state.created_at,
                            );
                        let metadata_path = session_dir.join(&metadata_filename);
                        let metadata_exists = match tokio::fs::try_exists(&metadata_path).await {
                            Ok(exists) => exists,
                            Err(err) => {
                                warn!(?err, "failed to check session metadata");
                                true
                            }
                        };
                        if metadata_exists {
                            match session::load_session_metadata(&session_dir, &metadata_filename)
                                .await
                            {
                                Ok(metadata) => metadata.summary,
                                Err(err) => {
                                    warn!(?err, "failed to load session metadata");
                                    None
                                }
                            }
                        } else {
                            generate_session_summary(
                                &state.messages,
                                state.model_override.clone(),
                                state.thinking_enabled,
                            )
                            .await
                        }
                    };
                    let now = time::OffsetDateTime::now_utc();
                    let persistent_session = crate::session::PersistentSession {
                        name: state.name.clone(),
                        inner: session::save_related(&state, messages),
                        created_at: state.created_at,
                        updated_at: now,
                    };

                    if let Err(e) =
                        crate::session::save_session(&session_dir, persistent_session, summary)
                            .await
                    {
                        warn!(?e, "failed to save session");
                    } else {
                        debug!("Session saved successfully");
                    }
                }
            }
        });
    }

    fn save_at(&mut self, save_at: Instant) {
        self.schedule_save_task(save_at, false);
    }

    fn save_now(&mut self) {
        self.schedule_save_task(Instant::now(), false);
    }

    fn regenerate_session_summary(&mut self) {
        self.state.write_untracked().updated_at = OffsetDateTime::now_utc();
        self.schedule_save_task(Instant::now(), true);
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
            | ComboEvent::PromptStream { .. }
            | ComboEvent::PromptStreamReset { .. } => {
                self.set_processing();
            }
            ComboEvent::Prompt { thinking, .. } => {
                self.set_combo_thinking_active(thinking.as_ref().is_some_and(|cfg| cfg.enabled));
                self.set_processing();
            }
            ComboEvent::ReplyToolUse { offload: false, .. } => {
                self.set_combo_thinking_active(false);
                self.set_processing();
            }
            ComboEvent::ReplyToolUse { offload: true, .. } => {
                self.set_processing();
            }
            ComboEvent::Executed { id, starter, .. } => {
                self.set_combo_thinking_active(false);
                if let Err(err) = starter.combo.as_ref() {
                    warn!(?err, "Failed to execute starter");
                }
                if !self.manual_combo_runs.contains(id) {
                    self.spawn_chat_with_history();
                }
            }
            ComboEvent::Discovered { .. }
            | ComboEvent::NotFound { .. }
            | ComboEvent::Cancelled { .. } => {
                self.set_combo_thinking_active(false);
                self.set_ready();
            }
            ComboEvent::ReplyToolError { message } => {
                self.set_combo_thinking_active(false);
                self.messages
                    .push(Message::system(Plain::new(message.to_string()).into()));
                // Set chat status to Ready after combo tool error
                self.set_ready();
                global::trigger_schedule_session_save();
            }
            ComboEvent::ReplyToolResult { .. } => {
                // Result is handled by Combo component
            }
            ComboEvent::Transcript { id, name, messages } => {
                self.store_combo_transcript(id.clone(), name.clone(), messages.clone());
            }
        }
    }

    fn set_combo_thinking_active(&mut self, enabled: bool) {
        if self.combo_thinking_active == enabled {
            return;
        }
        self.combo_thinking_active = enabled;
        global::signal_dirty();
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

    fn spawn_combo_execute(&mut self, id: String, name: String, args: Vec<String>) {
        self.set_processing();
        self.register_combo_tool_message(id.clone());
        let cancel_token = self.cancellation_guard.token();
        let agent = self.agent.clone();
        tokio::task::spawn(task_combo_execute(agent, id, name, args, cancel_token));
    }

    fn dispatch_combo_event(&mut self, event: ComboEvent) {
        self.forward_combo_event_to_session(&event);
        self.handle_combo_event_from_tool(&event);
        let combo_event_wrapped = Event::Combo(event);
        let combo_event_ref = &combo_event_wrapped;
        handle_component_event!(self, combo_event_ref);
    }

    fn forward_combo_event_to_session(&self, event: &ComboEvent) {
        let Some(run_id) = Self::combo_event_run_id(event) else {
            return;
        };
        let Some(bridge) = global::combo_run_bridge() else {
            return;
        };
        if !bridge.contains(run_id) {
            return;
        }
        let run_event = Self::combo_event_to_run_event(event);
        bridge.send_event(run_id, run_event);
    }

    fn forward_combo_result_to_session(&self, run_id: &str, output: &Final, is_error: bool) {
        let Some(bridge) = global::combo_run_bridge() else {
            return;
        };
        if !bridge.contains(run_id) {
            return;
        }
        let result = combo_run_result_from_final(run_id, output, is_error);
        bridge.send_result(run_id, result);
    }

    fn register_combo_tool_message(&mut self, id: String) {
        self.combo_tool_messages.insert(id.clone());
        if let Some(events) = self.pending_combo_tool_events.remove(&id) {
            for event in events {
                self.dispatch_combo_event(event);
            }
        }
    }

    fn combo_tool_event_to_combo_event(&self, id: &str, event: &ComboToolEvent) -> ComboEvent {
        match event {
            ComboToolEvent::NotFound { name } => ComboEvent::NotFound {
                id: id.to_string(),
                name: name.clone(),
            },
            ComboToolEvent::Executing { name, command_line } => ComboEvent::Executing {
                id: id.to_string(),
                name: name.clone(),
                command_line: command_line.clone(),
            },
            ComboToolEvent::Output { name, chunk } => ComboEvent::Output {
                id: id.to_string(),
                name: name.clone(),
                chunk: chunk.clone(),
            },
            ComboToolEvent::RecordStart { name, tool_use } => ComboEvent::RecordStart {
                id: id.to_string(),
                name: name.clone(),
                tool_use: tool_use.clone(),
            },
            ComboToolEvent::RecordOutput {
                name,
                tool_use_id,
                chunk,
            } => ComboEvent::RecordOutput {
                id: id.to_string(),
                name: name.clone(),
                tool_use_id: tool_use_id.clone(),
                chunk: chunk.clone(),
            },
            ComboToolEvent::RecordEnd {
                name,
                tool_use_id,
                is_error,
                output,
            } => ComboEvent::RecordEnd {
                id: id.to_string(),
                name: name.clone(),
                tool_use_id: tool_use_id.clone(),
                is_error: *is_error,
                output: output.clone(),
            },
            ComboToolEvent::Prompt {
                name,
                prompt,
                thinking,
            } => ComboEvent::Prompt {
                id: id.to_string(),
                name: name.clone(),
                prompt: prompt.clone(),
                thinking: thinking.clone(),
            },
            ComboToolEvent::PromptStream {
                name,
                index,
                kind,
                text,
            } => ComboEvent::PromptStream {
                id: id.to_string(),
                name: name.clone(),
                index: *index,
                kind: match kind {
                    ComboToolStreamKind::Plain => BotStreamKind::Plain,
                    ComboToolStreamKind::Thinking => BotStreamKind::Thinking,
                },
                text: text.clone(),
            },
            ComboToolEvent::PromptStreamReset { name } => ComboEvent::PromptStreamReset {
                id: id.to_string(),
                name: name.clone(),
            },
            ComboToolEvent::ReplyToolUse {
                name,
                tool_use,
                thinking,
                offload,
            } => ComboEvent::ReplyToolUse {
                id: id.to_string(),
                name: name.clone(),
                tool_use: tool_use.clone(),
                thinking: thinking.clone(),
                offload: *offload,
            },
            ComboToolEvent::ReplyToolResult {
                name,
                tool_use_id,
                is_error,
                output,
            } => ComboEvent::ReplyToolResult {
                id: id.to_string(),
                name: name.clone(),
                tool_use_id: tool_use_id.clone(),
                is_error: *is_error,
                output: output.clone(),
            },
            ComboToolEvent::ReplyToolError { message } => ComboEvent::ReplyToolError {
                message: message.clone(),
            },
            ComboToolEvent::Executed {
                name,
                starter,
                exit_code,
            } => ComboEvent::Executed {
                id: id.to_string(),
                name: name.clone(),
                starter: starter.clone(),
                exit_code: *exit_code,
            },
            ComboToolEvent::Cancelled { name } => ComboEvent::Cancelled {
                id: Some(id.to_string()),
                name: name.clone(),
            },
            ComboToolEvent::Transcript { name, messages } => ComboEvent::Transcript {
                id: id.to_string(),
                name: name.clone(),
                messages: messages.clone(),
            },
        }
    }

    fn combo_event_to_run_event(event: &ComboEvent) -> ComboRunEvent {
        match event {
            ComboEvent::Discovering => ComboRunEvent::Discovering,
            ComboEvent::Discovered { starters } => ComboRunEvent::Discovered {
                starters: starters.clone(),
            },
            ComboEvent::Executing {
                id,
                name,
                command_line,
            } => ComboRunEvent::Executing {
                id: id.clone(),
                name: name.clone(),
                command_line: command_line.clone(),
            },
            ComboEvent::RecordStart { id, name, tool_use } => ComboRunEvent::RecordStart {
                id: id.clone(),
                name: name.clone(),
                tool_use: tool_use.clone(),
            },
            ComboEvent::Output { id, name, chunk } => ComboRunEvent::Output {
                id: id.clone(),
                name: name.clone(),
                chunk: chunk.clone(),
            },
            ComboEvent::RecordOutput {
                id,
                name,
                tool_use_id,
                chunk,
            } => ComboRunEvent::RecordOutput {
                id: id.clone(),
                name: name.clone(),
                tool_use_id: tool_use_id.clone(),
                chunk: chunk.clone(),
            },
            ComboEvent::RecordEnd {
                id,
                name,
                tool_use_id,
                is_error,
                output,
            } => ComboRunEvent::RecordEnd {
                id: id.clone(),
                name: name.clone(),
                tool_use_id: tool_use_id.clone(),
                is_error: *is_error,
                output: output.clone(),
            },
            ComboEvent::Prompt {
                id,
                name,
                prompt,
                thinking,
            } => ComboRunEvent::Prompt {
                id: id.clone(),
                name: name.clone(),
                prompt: prompt.clone(),
                thinking: thinking.clone(),
            },
            ComboEvent::PromptStream {
                id,
                name,
                index,
                kind,
                text,
            } => ComboRunEvent::PromptStream {
                id: id.clone(),
                name: name.clone(),
                index: *index,
                kind: match kind {
                    BotStreamKind::Plain => ComboRunStreamKind::Plain,
                    BotStreamKind::Thinking => ComboRunStreamKind::Thinking,
                },
                text: text.clone(),
            },
            ComboEvent::PromptStreamReset { id, name } => ComboRunEvent::PromptStreamReset {
                id: id.clone(),
                name: name.clone(),
            },
            ComboEvent::ReplyToolUse {
                id,
                name,
                tool_use,
                thinking,
                offload,
            } => ComboRunEvent::ReplyToolUse {
                id: id.clone(),
                name: name.clone(),
                tool_use: tool_use.clone(),
                thinking: thinking.clone(),
                offload: *offload,
            },
            ComboEvent::ReplyToolResult {
                id,
                name,
                tool_use_id,
                is_error,
                output,
            } => ComboRunEvent::ReplyToolResult {
                id: id.clone(),
                name: name.clone(),
                tool_use_id: tool_use_id.clone(),
                is_error: *is_error,
                output: output.clone(),
            },
            ComboEvent::Executed {
                id,
                name,
                starter,
                exit_code,
            } => ComboRunEvent::Executed {
                id: id.clone(),
                name: name.clone(),
                starter: starter.clone(),
                exit_code: *exit_code,
            },
            ComboEvent::ReplyToolError { message } => ComboRunEvent::ReplyToolError {
                message: message.clone(),
            },
            ComboEvent::NotFound { id, name } => ComboRunEvent::NotFound {
                id: id.clone(),
                name: name.clone(),
            },
            ComboEvent::Cancelled { id, name } => ComboRunEvent::Cancelled {
                id: id.clone(),
                name: name.clone(),
            },
            ComboEvent::Transcript { id, name, messages } => ComboRunEvent::Transcript {
                id: id.clone(),
                name: name.clone(),
                messages: messages.clone(),
            },
        }
    }

    fn combo_event_run_id(event: &ComboEvent) -> Option<&str> {
        match event {
            ComboEvent::Discovering | ComboEvent::Discovered { .. } => None,
            ComboEvent::ReplyToolError { .. } => None,
            ComboEvent::Cancelled { id, .. } => id.as_deref(),
            ComboEvent::Executing { id, .. }
            | ComboEvent::RecordStart { id, .. }
            | ComboEvent::Output { id, .. }
            | ComboEvent::RecordOutput { id, .. }
            | ComboEvent::RecordEnd { id, .. }
            | ComboEvent::Prompt { id, .. }
            | ComboEvent::PromptStream { id, .. }
            | ComboEvent::PromptStreamReset { id, .. }
            | ComboEvent::ReplyToolUse { id, .. }
            | ComboEvent::ReplyToolResult { id, .. }
            | ComboEvent::Executed { id, .. }
            | ComboEvent::NotFound { id, .. }
            | ComboEvent::Transcript { id, .. } => Some(id.as_str()),
        }
    }

    fn handle_combo_event_from_tool(&mut self, event: &ComboEvent) {
        match event {
            ComboEvent::Executing {
                id, command_line, ..
            } => {
                if self.manual_combo_runs.contains(id) {
                    if !self.manual_combo_commands.contains_key(id) {
                        self.manual_combo_commands
                            .insert(id.clone(), command_line.clone());
                    }
                    let command_line = self
                        .manual_combo_commands
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| command_line.clone());
                    self.ensure_manual_combo_tool_use(id, &command_line);
                }
                self.handle_combo_event(event);
            }
            ComboEvent::Executed { id, starter, .. } => {
                self.set_combo_thinking_active(false);
                if let Err(err) = starter.combo.as_ref() {
                    warn!(?err, "Failed to execute starter");
                }
                if !self.manual_combo_runs.contains(id) {
                    self.spawn_chat_with_history();
                }
            }
            _ => self.handle_combo_event(event),
        }
    }

    fn ensure_manual_combo_tool_use(&mut self, id: &str, command_line: &str) {
        if !self.manual_combo_runs.contains(id) {
            return;
        }
        self.ensure_manual_combo_prompt(id, command_line);
        if !self.manual_combo_tool_uses.insert(id.to_string()) {
            return;
        }
        let tool_use = ToolUse {
            id: id.to_string(),
            name: BASH_TOOL_NAME.to_string(),
            input: serde_json::json!({ "command": command_line }),
        };
        let message =
            ChatMessage::assistant(ChatContent::Multiple(vec![ChatBlock::ToolUse(tool_use)]));
        self.append_agent_message_sync(message);
    }

    fn ensure_manual_combo_prompt(&mut self, id: &str, command_line: &str) {
        if !self.manual_combo_runs.contains(id) {
            return;
        }
        if !self.manual_combo_prompted.insert(id.to_string()) {
            return;
        }
        let prompt = format!("User use combo {command_line}");
        let message = ChatMessage::user(ChatContent::Text(prompt));
        self.append_agent_message_sync(message);
    }

    fn append_agent_message_sync(&self, message: ChatMessage) {
        let agent = self.agent.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                agent.append_message(message).await;
            });
        });
    }

    fn format_combo_run_command(&self, name: &str, args: &[String]) -> String {
        let mut parts = Vec::with_capacity(args.len() + 4);
        parts.push("coco".to_string());
        parts.push("combo".to_string());
        parts.push("run".to_string());
        parts.push(display_combo_arg(name));
        for arg in args {
            parts.push(display_combo_arg(arg));
        }
        parts.join(" ")
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

    fn update_terminal_focused(&mut self, focused: bool) {
        if self.terminal_focused == focused {
            return;
        }
        self.terminal_focused = focused;
        debug!(focused, "terminal focus changed");
    }

    fn notify_reply_ready(&self, summary: Option<&str>) {
        let config = global::config_sync();
        if !self.should_notify(&config) {
            return;
        }
        let body = match summary {
            Some(text) if !text.trim().is_empty() => format!("Reply ready: {text}"),
            _ => "Reply ready".to_string(),
        };
        notifications::send_notification(NOTIFY_TITLE, &body, &config.ui.notifications.backend);
    }

    fn notify_action_required(&self, reason: &str) {
        let config = global::config_sync();
        if !self.should_notify(&config) {
            return;
        }
        let body = if reason.trim().is_empty() {
            "Action required".to_string()
        } else {
            format!("Action required: {reason}")
        };
        notifications::send_notification(NOTIFY_TITLE, &body, &config.ui.notifications.backend);
    }

    fn should_notify(&self, config: &Config) -> bool {
        if !config.ui.notifications.enabled {
            return false;
        }
        if config.ui.notifications.only_when_unfocused && self.terminal_focused {
            debug!(
                focused = true,
                "notification suppressed: only_when_unfocused enabled"
            );
            return false;
        }
        true
    }

    fn reply_summary_from_messages(msgs: &[BotMessage]) -> Option<String> {
        let mut summary = None;
        for msg in msgs {
            if let BotMessage::Plain(text) = msg
                && let Some(line) = Self::first_non_empty_line(text)
            {
                summary = Some(line);
            }
        }
        summary
    }

    fn first_non_empty_line(text: &str) -> Option<String> {
        for line in text.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        None
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
        self.messages.collapse_thinking();
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
        self.persist_runtime_overrides();
        global::trigger_schedule_session_save();
    }

    fn toggle_thinking(&mut self) {
        let enabled = {
            let mut state = self.state.write();
            state.thinking_enabled = !state.thinking_enabled;
            state.thinking_enabled
        };
        self.agent.set_thinking_enabled(enabled);
        self.persist_runtime_overrides();
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
        hints.push_hidden(&[("Thinking", "C-r")]);
        hints.push_hidden(&[("Auto Accept Edits", "S-Tab")]);
        hints
    }

    fn chat_messages_shortcut_hints(&self) -> ShortcutHints {
        let mut hints = self.messages.shortcut_hints();
        if self.messages.has_thinking_toggle_for_focus() {
            hints.push_visible(&[("Thinking", "r")]);
        }
        if !self.messages.is_actionable() {
            hints.push_visible(&[("Back", "Esc")]);
        }
        hints.push_visible(&[("Up", "k"), ("Down", "j")]);
        hints.push_hidden(&[("Scroll Up", "C-y"), ("Down", "C-e")]);
        hints.push_hidden(&[("Scroll+ Up", "C-u"), ("Down", "C-d")]);
        hints.push_hidden(&[("Thinking", "C-r")]);
        hints.push_hidden(&[("Auto Accept Edits", "S-Tab")]);
        hints
    }

    fn transcript_shortcut_hints(&self) -> ShortcutHints {
        let mut hints = self.transcript.shortcut_hints();
        let back_label = if self.transcript_scopes.is_empty() {
            "Close"
        } else {
            "Back"
        };
        hints.push_visible(&[(back_label, "Esc")]);
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
            .title_bottom(self.widget_state_indicator());
        if let Some(line) = self.retry_indicator_line() {
            block = block.title_bottom(line);
        }
        block = block
            .title_bottom(self.model_indicator())
            .title_bottom(self.auto_accept_indicator())
            .title_bottom(self.thinking_indicator());
        if let Some(line) = self.ctrl_c_reminder_line() {
            block = block.title_bottom(line);
        }
        if let Some(line) = self.context_usage_indicator() {
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

    fn retry_indicator_line(&self) -> Option<Line<'static>> {
        let retry = self.retry_status.as_ref()?;
        let delay_ms = retry.delay.as_millis();
        let delay_text = if delay_ms >= 1000 {
            format!("{:.1}s", retry.delay.as_secs_f64())
        } else {
            format!("{delay_ms}ms")
        };
        let theme = global::theme();
        let text = format!(
            " retrying in {delay_text} (attempt {}/{}) ",
            retry.attempt, retry.max_attempts
        );
        Some(Line::from(Span::styled(text, theme.ui.status_warning)))
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

    fn model_indicator(&self) -> Line<'static> {
        let theme = global::theme();
        let model = self.agent.current_model();
        Line::from(vec![
            Span::styled(" model: ", theme.ui.shortcut_desc),
            Span::styled(model, theme.ui.shortcut),
            Span::raw(" "),
        ])
    }

    fn context_usage_indicator(&self) -> Option<Line<'static>> {
        let usage = self.last_usage.as_ref()?;
        let input_tokens = usage.input_tokens.unwrap_or(0);
        let output_tokens = usage.output_tokens.unwrap_or(0);
        let used_tokens = usage.total_tokens.or_else(|| {
            if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
                Some(input_tokens.saturating_add(output_tokens))
            } else {
                None
            }
        })?;
        let theme = global::theme();
        let text = if let Some(window) = self.agent.context_window() {
            let percent = if window == 0 {
                0
            } else {
                ((used_tokens as f64 / window as f64) * 100.0).round() as usize
            };
            format!(" ctx: {used_tokens}/{window} ({percent}%) ")
        } else {
            format!(" ctx: {used_tokens} tok ")
        };
        Some(Line::from(Span::styled(text, theme.ui.shortcut_desc)).alignment(Alignment::Right))
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

    fn thinking_indicator(&self) -> Line<'static> {
        let theme = global::theme();
        let (status, status_style, origin) = if self.combo_thinking_active {
            ("on", theme.ui.auto_accept_on, Some("combo"))
        } else if self.state.thinking_enabled {
            ("on", theme.ui.auto_accept_on, None)
        } else {
            ("off", theme.ui.auto_accept_off, None)
        };
        let mut spans = vec![
            Span::styled(" thinking: ", theme.ui.shortcut_desc),
            Span::styled(status, status_style),
            Span::raw(" "),
        ];
        if let Some(origin) = origin {
            spans.push(Span::styled(format!("({origin}) "), theme.ui.shortcut_desc));
        }
        spans.push(Span::styled("<C-r>", theme.ui.shortcut));
        spans.push(Span::raw(" "));
        Line::from(spans)
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
            // Set chat status to Ready after all hunks rejected
            self.set_ready();
        }
    }

    fn open_transcript(&mut self) {
        self.update_focus(Focus::InputBlur);
        self.transcript_scopes.clear();
        self.rebuild_transcript_view();
        self.view = ViewMode::Transcript;
        global::signal_dirty();
    }

    fn close_transcript(&mut self) {
        self.view = ViewMode::Chat;
        self.update_focus(Focus::InputBlur);
        self.transcript_scopes.clear();
        global::signal_dirty();
    }

    fn rebuild_transcript_view(&mut self) {
        self.transcript.clear();
        let scope = self.transcript_scopes.last().cloned();
        match scope {
            None => self.build_root_transcript(),
            Some(TranscriptScope::Combo { id, name }) => {
                self.build_combo_transcript(&id, &name);
            }
            Some(TranscriptScope::Subagent { id, name }) => {
                self.build_subagent_transcript(&id, &name);
            }
        }
        self.ensure_transcript_focus();
    }

    fn build_root_transcript(&mut self) {
        let agent_messages = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.agent.dump_messages())
        });
        let link_map = self.build_transcript_link_map(&agent_messages);
        self.append_transcript_messages(agent_messages, &link_map);
    }

    fn build_combo_transcript(&mut self, id: &str, name: &str) {
        let state = self.state.read();
        let Some(entry) = state.combo_transcripts.iter().find(|entry| entry.id == id) else {
            let message = format!("Combo transcript not found: {} ({})", name, id);
            self.transcript
                .push(Message::system(Plain::new(message).into()).with_role_prefix(false));
            return;
        };
        let messages = entry.messages.clone();
        let link_map = self.build_transcript_link_map(&messages);
        self.append_transcript_messages(messages, &link_map);
    }

    fn build_subagent_transcript(&mut self, id: &str, name: &str) {
        let state = self.state.read();
        let Some(entry) = state
            .subagent_transcripts
            .iter()
            .find(|entry| entry.id == id)
        else {
            let message = format!("Subagent transcript not found: {} ({})", name, id);
            self.transcript
                .push(Message::system(Plain::new(message).into()).with_role_prefix(false));
            return;
        };
        let messages = entry.messages.clone();
        let link_map = self.build_transcript_link_map(&messages);
        self.append_transcript_messages(messages, &link_map);
    }

    fn append_transcript_messages(
        &mut self,
        messages: Vec<ChatMessage>,
        link_map: &HashMap<String, TranscriptLinkTarget>,
    ) {
        let iter = messages.into_iter().map(|message| {
            Message::system(TranscriptMessage::new_with_links(message, link_map).into())
        });
        self.transcript.extend(iter);
    }

    fn build_transcript_link_map(
        &self,
        messages: &[ChatMessage],
    ) -> HashMap<String, TranscriptLinkTarget> {
        let mut map = HashMap::new();
        for message in messages {
            let ChatContent::Multiple(blocks) = &message.content else {
                continue;
            };
            for block in blocks {
                let ChatBlock::ToolUse(tool_use) = block else {
                    continue;
                };
                match tool_use.name.as_str() {
                    RUN_COMBO_TOOL_NAME => {
                        if let Ok(input) =
                            serde_json::from_value::<RunComboInput>(tool_use.input.clone())
                        {
                            map.insert(
                                tool_use.id.clone(),
                                TranscriptLinkTarget {
                                    kind: TranscriptLinkKind::Combo,
                                    id: tool_use.id.clone(),
                                    name: input.combo_name,
                                },
                            );
                        }
                    }
                    BASH_TOOL_NAME => {
                        if let Some(name) = self.combo_name_from_bash_tool_use(tool_use) {
                            map.insert(
                                tool_use.id.clone(),
                                TranscriptLinkTarget {
                                    kind: TranscriptLinkKind::Combo,
                                    id: tool_use.id.clone(),
                                    name,
                                },
                            );
                        }
                    }
                    RUN_TASK_TOOL_NAME => {
                        if let Ok(input) =
                            serde_json::from_value::<RunTaskInput>(tool_use.input.clone())
                        {
                            map.insert(
                                tool_use.id.clone(),
                                TranscriptLinkTarget {
                                    kind: TranscriptLinkKind::Subagent,
                                    id: tool_use.id.clone(),
                                    name: input.subagent_name,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        map
    }

    fn combo_name_from_bash_tool_use(&self, tool_use: &ToolUse) -> Option<String> {
        let input = serde_json::from_value::<BashInput>(tool_use.input.clone()).ok()?;
        let name = self.parse_combo_name_from_command(&input.command)?;
        Some(
            self.combo_name_from_transcript(&tool_use.id)
                .unwrap_or(name),
        )
    }

    fn combo_name_from_transcript(&self, id: &str) -> Option<String> {
        let state = self.state.read();
        state
            .combo_transcripts
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.clone())
    }

    fn parse_combo_name_from_command(&self, command: &str) -> Option<String> {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        for (idx, token) in tokens.iter().enumerate() {
            if !is_combo_command_token(token)
                || tokens.get(idx + 1) != Some(&"combo")
                || tokens.get(idx + 2) != Some(&"run")
            {
                continue;
            }
            let mut name_idx = idx + 3;
            if tokens.get(name_idx) == Some(&"--") {
                name_idx += 1;
            }
            let name = tokens.get(name_idx)?;
            return Some(trim_combo_token(name));
        }
        None
    }

    fn ensure_transcript_focus(&mut self) {
        if self.transcript.selected_idx().is_none() && !self.transcript.is_empty() {
            self.transcript.focus(0);
        }
    }

    fn move_transcript_selection(&mut self, key: NavigationKey) {
        if self.transcript.selected_idx().is_none() {
            match key {
                NavigationKey::Down => {
                    let _ = self.transcript.focus(0);
                }
                NavigationKey::Up => {
                    let _ = self.transcript.select_last();
                }
            }
            return;
        }
        let _ = self.transcript.handle_navigation(key);
    }

    fn transcript_selected_link_target(&self) -> Option<TranscriptLinkTarget> {
        self.transcript
            .selected_message_as::<TranscriptMessage>()
            .and_then(|message| message.link_target())
    }

    fn open_transcript_link(&mut self) {
        let Some(target) = self.transcript_selected_link_target() else {
            return;
        };
        let scope = match target.kind {
            TranscriptLinkKind::Combo => TranscriptScope::Combo {
                id: target.id,
                name: target.name,
            },
            TranscriptLinkKind::Subagent => TranscriptScope::Subagent {
                id: target.id,
                name: target.name,
            },
        };
        self.transcript_scopes.push(scope);
        self.rebuild_transcript_view();
    }

    fn back_or_close_transcript(&mut self) {
        if self.transcript_scopes.pop().is_some() {
            self.rebuild_transcript_view();
        } else {
            self.close_transcript();
        }
    }

    fn transcript_breadcrumb_line(&self) -> Line<'static> {
        let theme = global::theme();
        let mut crumbs = vec!["Transcript".to_string()];
        if self.transcript_scopes.is_empty() {
            crumbs.push("Root".to_string());
        } else {
            for scope in &self.transcript_scopes {
                let label = match scope {
                    TranscriptScope::Combo { name, .. } => format!("Combo: {}", name),
                    TranscriptScope::Subagent { name, .. } => format!("Subagent: {}", name),
                };
                crumbs.push(label);
            }
        }
        let breadcrumb = crumbs.join(" / ");
        Line::from(Span::styled(
            format!(" {} ", breadcrumb),
            theme.ui.folded_hint,
        ))
    }

    fn store_combo_transcript(&mut self, id: String, name: String, messages: Vec<ChatMessage>) {
        if messages.is_empty() {
            return;
        }
        let should_refresh = if self.view == ViewMode::Transcript {
            match self.transcript_scopes.last() {
                None => true,
                Some(TranscriptScope::Combo { id: scope_id, .. }) => scope_id == &id,
                _ => false,
            }
        } else {
            false
        };
        {
            let mut state = self.state.write();
            if let Some(existing) = state
                .combo_transcripts
                .iter_mut()
                .find(|entry| entry.id == id)
            {
                existing.name = name;
                existing.messages = messages;
            } else {
                state
                    .combo_transcripts
                    .push(ComboTranscript { id, name, messages });
            }
        }
        global::trigger_schedule_session_save();
        if should_refresh {
            self.rebuild_transcript_view();
        }
    }

    fn store_subagent_transcript(&mut self, id: String, name: String, messages: Vec<ChatMessage>) {
        if messages.is_empty() {
            return;
        }
        let should_refresh = if self.view == ViewMode::Transcript {
            match self.transcript_scopes.last() {
                None => true,
                Some(TranscriptScope::Subagent { id: scope_id, .. }) => scope_id == &id,
                _ => false,
            }
        } else {
            false
        };
        {
            let mut state = self.state.write();
            if let Some(existing) = state
                .subagent_transcripts
                .iter_mut()
                .find(|entry| entry.id == id)
            {
                existing.name = name;
                existing.messages = messages;
            } else {
                state
                    .subagent_transcripts
                    .push(SubagentTranscript { id, name, messages });
            }
        }
        global::trigger_schedule_session_save();
        if should_refresh {
            self.rebuild_transcript_view();
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

        self.set_combo_thinking_active(false);

        let auto_accept_edits = self.agent.auto_accept_edits();
        let thinking_enabled = self.agent.thinking_enabled();
        let model_override = self.agent.model_override().map(|model| model.to_string());

        // 2. Clear messages
        self.messages.clear();

        // 3. Reset state
        let mut state = self.state.write();
        *state = Inner::default();
        state.auto_accept_edits = auto_accept_edits;
        state.thinking_enabled = thinking_enabled;
        state.model_override = model_override;
        self.cancellation_guard.reset();

        // 4. Cancel any pending save timer
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

    fn switch_model(&mut self, model_override: Option<&String>) {
        let model_override = model_override.cloned();
        self.state.write().model_override = model_override.clone();
        self.agent.set_model_override(model_override);
        self.last_usage = None;
        self.persist_runtime_overrides();
        global::trigger_schedule_session_save();
    }

    fn persist_runtime_overrides(&self) {
        let config = global::config_sync();
        let state = self.state.read();
        let overrides = RuntimeOverrides {
            model_override: state.model_override.clone(),
            thinking_enabled: Some(state.thinking_enabled),
            auto_accept_edits: Some(state.auto_accept_edits),
        };
        if let Err(err) = save_runtime_overrides(&config.config_dir, &overrides) {
            warn!(?err, "failed to persist runtime overrides");
        }
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
        let thinking_enabled = state.thinking_enabled;
        let model_override = state.model_override.clone();

        if state.focus == Focus::ShortcutHints {
            state.focus = Focus::InputBlur;
        }

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(inst.agent.restore_messages(&state.messages));
            state.messages.clear();
        });

        // Restore system prompt from persisted value if available
        if let Some(saved_prompt) = state.system_prompt.take() {
            inst.agent.set_system_prompt(&saved_prompt);
        }

        inst.agent.set_auto_accept_edits(auto_accept_edits);
        inst.agent.set_thinking_enabled(thinking_enabled);
        inst.agent.set_model_override(model_override);

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
            Event::FocusGained => {
                self.update_terminal_focused(true);
            }
            Event::FocusLost => {
                self.update_terminal_focused(false);
            }
            Event::Combo(combo) => {
                // Update agent's combo list when combos are discovered
                if let ComboEvent::Discovered { starters } = combo {
                    let combos: Vec<ComboInfo> = starters
                        .iter()
                        .filter_map(|s| {
                            s.combo.as_ref().ok().map(|c| ComboInfo {
                                path: s.path.clone(),
                                combo: c.clone(),
                            })
                        })
                        .collect();
                    let agent = self.agent.clone();
                    tokio::spawn(async move {
                        agent.set_combos(combos).await;
                    });
                }
                self.handle_combo_event(combo);
                // Combo events need to be handled by children components
                handle_component_event!(self, event);
            }
            Event::Ask(AskEvent::Bot) => {
                self.set_processing();
            }
            Event::Answer(AnswerEvent::Bot(msgs)) => {
                self.messages.finalize_stream();
                self.messages.reset_stream();
                // Check if there are any tool uses that will be executed
                let has_tool_use = msgs.iter().any(|m| matches!(m, BotMessage::ToolUse(_)));
                let reply_summary = Self::reply_summary_from_messages(msgs);
                if !has_tool_use {
                    // Only set ready if no tools to execute
                    self.set_ready();
                    self.notify_reply_ready(reply_summary.as_deref());
                }
                let mut new_messages = Vec::with_capacity(msgs.len());
                let mut combo_tool_ids = Vec::new();
                for msg in msgs.iter().cloned() {
                    let message = match msg {
                        BotMessage::Plain(text) => Message::bot(Plain::new(text).into()),
                        BotMessage::ToolUse(tool_use) => {
                            if tool_use.name == RUN_COMBO_TOOL_NAME {
                                if let Ok(input) =
                                    serde_json::from_value::<RunComboInput>(tool_use.input.clone())
                                {
                                    combo_tool_ids.push(tool_use.id.clone());
                                    Message::bot(Combo::new(&tool_use.id, &input.combo_name).into())
                                } else {
                                    Message::bot(Tool::new(tool_use.to_owned()).into())
                                }
                            } else {
                                Message::bot(Tool::new(tool_use.to_owned()).into())
                            }
                        }
                        BotMessage::System(message) => Message::system(Plain::new(message).into()),
                        BotMessage::Thinking(thinking) => {
                            Message::bot(Thinking::new(thinking).into())
                        }
                    };
                    new_messages.push(message);
                }
                self.messages.extend(new_messages.into_iter());
                for id in combo_tool_ids {
                    self.register_combo_tool_message(id);
                }
                // Trigger session save after receiving bot response
                global::trigger_schedule_session_save();
            }
            Event::Answer(AnswerEvent::BotStreamReset) => {
                self.messages.reset_stream();
            }
            Event::Answer(AnswerEvent::BotStream { index, kind, text }) => {
                self.messages
                    .append_stream_text(*index, *kind, text.clone());
            }
            Event::Answer(AnswerEvent::Usage { usage }) => {
                match &mut self.last_usage {
                    Some(total) => add_usage(total, usage),
                    None => {
                        self.last_usage = Some(usage.clone());
                    }
                }
                global::signal_dirty();
            }
            Event::Answer(AnswerEvent::RetryUpdate { update }) => {
                match update {
                    RetryUpdate::Attempt(attempt) => {
                        self.retry_status = Some(attempt.clone());
                    }
                    RetryUpdate::Finished { .. } => {
                        self.retry_status = None;
                    }
                }
                global::signal_dirty();
            }
            Event::Answer(AnswerEvent::Cancelled) => {
                self.messages.reset_stream();
                self.set_ready();
            }
            Event::Ask(AskEvent::ToolUsePermission(_)) => {
                if let Some(idx) = self.messages.on_tool_event(event) {
                    // Move focus to tool use message when permission is required
                    self.update_focus(Focus::Messages);
                    self.messages.focus(idx);
                }
                self.notify_action_required("Tool permission requested");
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
                    self.notify_action_required("Text edit confirmation required");
                }
                // Trigger session save after ask event
                global::trigger_schedule_session_save();
            }
            Event::Answer(AnswerEvent::PendingToolExecutions { ids }) => {
                // Track expected tool IDs for concurrent execution batching
                debug!(
                    ids_cnt = ids.len(),
                    "received pending tool executions notification"
                );
                self.pending_tool_ids = ids.iter().cloned().collect();
                self.collected_tool_results.clear();
            }
            Event::Answer(AnswerEvent::ToolResult {
                id,
                is_user_cancelled,
                is_error,
                output,
            }) => {
                let is_manual_combo = self.manual_combo_runs.contains(id);
                self.forward_combo_result_to_session(id, output, *is_error);
                if let Some(idx) = self.messages.on_tool_event(event)
                    && !is_error
                    && self.messages.selected_idx() == Some(idx)
                {
                    // Move focus back to Input if tool use success.
                    self.update_focus(Focus::Input);
                    self.messages.blur();
                }

                if is_manual_combo {
                    let command_line = self
                        .manual_combo_commands
                        .remove(id)
                        .unwrap_or_else(|| format!("combo {id}"));
                    self.ensure_manual_combo_tool_use(id, &command_line);
                    let result = combo_run_result_from_final(id, output, *is_error);
                    let mut summary = result.summary.trim().to_string();
                    if summary.is_empty() {
                        summary = result
                            .error
                            .clone()
                            .unwrap_or_else(|| "Combo completed.".to_string());
                    }
                    let (stdout, stderr, exit_code) = if result.success {
                        (summary.clone(), String::new(), 0)
                    } else {
                        let stderr = result.error.clone().unwrap_or_else(|| summary.clone());
                        (summary.clone(), stderr, 1)
                    };
                    let bash_output = BashOutput {
                        exit_code,
                        stdout,
                        stderr,
                        timed_out: false,
                    };
                    let bash_value =
                        serde_json::to_value(bash_output).unwrap_or(Value::String(summary));
                    let content = final_to_tool_content(&Final::Json(bash_value));
                    let content = ChatContent::Multiple(vec![ChatBlock::ToolResult {
                        tool_use_id: id.clone(),
                        is_error: Some(!result.success),
                        content,
                    }]);
                    let content = self.build_user_content(content);
                    self.spawn_chat_task(content);
                    self.manual_combo_runs.remove(id);
                    self.manual_combo_tool_uses.remove(id);
                    self.manual_combo_prompted.remove(id);
                    global::trigger_schedule_session_save();
                    return;
                }

                // Build content for this tool result
                let mut content = final_to_tool_content(output);
                if *is_user_cancelled {
                    content = content.user_cancel();
                }

                // Check if we're collecting results for concurrent execution
                if self.pending_tool_ids.contains(id) {
                    // Collect this result
                    self.collected_tool_results.push(CollectedToolResult {
                        id: id.clone(),
                        is_error: *is_error,
                        content,
                    });
                    self.pending_tool_ids.remove(id);

                    debug!(
                        collected = self.collected_tool_results.len(),
                        remaining = self.pending_tool_ids.len(),
                        "collected tool result for concurrent batch"
                    );

                    // If all results collected, send them all together
                    if self.pending_tool_ids.is_empty() {
                        let blocks: Vec<ChatBlock> = self
                            .collected_tool_results
                            .drain(..)
                            .map(|r| code_combo::Block::ToolResult {
                                tool_use_id: r.id,
                                is_error: Some(r.is_error),
                                content: r.content,
                            })
                            .collect();
                        let content = ChatContent::Multiple(blocks);
                        let content = self.build_user_content(content);
                        self.spawn_chat_task(content);
                    }
                } else {
                    // Single tool execution or not tracked - process immediately
                    let content = ChatContent::Multiple(vec![code_combo::Block::ToolResult {
                        tool_use_id: id.clone(),
                        is_error: Some(*is_error),
                        content,
                    }]);
                    let content = self.build_user_content(content);
                    self.spawn_chat_task(content);
                }
                // Trigger session save after tool result
                global::trigger_schedule_session_save();
            }
            Event::Answer(AnswerEvent::ComboToolEvent { id, event }) => {
                let combo_event = self.combo_tool_event_to_combo_event(id, event);
                if !self.combo_tool_messages.contains(id) {
                    self.pending_combo_tool_events
                        .entry(id.clone())
                        .or_default()
                        .push(combo_event);
                    return;
                }
                self.dispatch_combo_event(combo_event);
            }
            evt @ Event::Answer(AnswerEvent::SubagentEvent { id, event }) => {
                if let SubagentEvent::Transcript {
                    subagent_name,
                    messages,
                } = event
                {
                    self.store_subagent_transcript(
                        id.clone(),
                        subagent_name.clone(),
                        messages.clone(),
                    );
                    return;
                }
                // RunTask tool is executing, set chat status to Processing
                self.set_processing();
                let _ = self.messages.on_tool_event(evt);
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
        if self.view == ViewMode::Chat
            && matches!(
                key,
                KeyEvent {
                    code: Char('r') | Char('R'),
                    modifiers: KM::CONTROL,
                    ..
                }
            )
        {
            self.toggle_thinking();
            return;
        }
        if self.view == ViewMode::Transcript {
            match (key.modifiers, key.code) {
                (KM::NONE, Esc) | (KM::NONE, Backspace) => self.back_or_close_transcript(),
                (KM::NONE, Enter) => self.open_transcript_link(),
                (KM::NONE, Char('k')) => {
                    self.move_transcript_selection(NavigationKey::Up);
                }
                (KM::NONE, Char('j')) => {
                    self.move_transcript_selection(NavigationKey::Down);
                }
                (KM::NONE, Up) => {
                    self.move_transcript_selection(NavigationKey::Up);
                }
                (KM::NONE, Down) => {
                    self.move_transcript_selection(NavigationKey::Down);
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
                let config_dir = global::config_sync().config_dir;
                let last_model_override = match load_runtime_overrides(&config_dir) {
                    Ok(overrides) => overrides.model_override,
                    Err(err) => {
                        warn!(?err, "failed to load runtime overrides");
                        None
                    }
                };
                let auto_model_label = Some(self.agent.resolved_default_model());
                self.command_palette.open(
                    self.state.created_at,
                    self.state.model_override.clone(),
                    last_model_override,
                    auto_model_label,
                );
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
            (Messages, KM::NONE, Char('r')) => {
                self.messages.toggle_thinking_for_focus();
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
                ComboAction::Execute { id, name, args } => {
                    let id = id
                        .clone()
                        .unwrap_or_else(|| format!("toolu_{}", Uuid::new_v4().as_simple()));
                    self.manual_combo_runs.insert(id.clone());
                    let command_line = self.format_combo_run_command(name, args);
                    self.manual_combo_commands.insert(id.clone(), command_line);
                    let combo = Combo::new(&id, name);
                    self.messages.push(Message::user(combo.into()));
                    self.spawn_combo_execute(id, name.clone(), args.clone());
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
                    // Set chat status to Ready after cancellation
                    self.set_ready();
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
                CommandPaletteAction::RegenerateSessionSummary => {
                    self.update_focus(Focus::InputBlur);
                    self.regenerate_session_summary();
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
                CommandPaletteAction::SwitchModel(model_override) => {
                    self.update_focus(Focus::InputBlur);
                    self.switch_model(model_override.as_ref());
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
        bottom_block = bottom_block.title_bottom(self.transcript_breadcrumb_line());
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

fn combo_discovery_dirs(config: &Config) -> Vec<PathBuf> {
    let mut combo_dirs = Vec::with_capacity(2);
    if !global::ignore_workspace_scripts() {
        combo_dirs.push(global::workspace_combo_dir());
    }
    combo_dirs.push(config.combo_dir());
    combo_dirs
}

async fn discover_combo_starters(
    cancel_token: CancellationToken,
    name: Option<&str>,
) -> Option<Vec<Starter>> {
    let tx = global::event_tx();
    let config = global::config().await;
    let combo_dirs = combo_discovery_dirs(&config);
    let combo_dirs = combo_dirs.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    tx.send(ComboEvent::Discovering.into()).unwrap();
    let result = discover_starters(&combo_dirs, cancel_token.clone()).await;
    if result.cancelled || cancel_token.is_cancelled() {
        tx.send(
            ComboEvent::Cancelled {
                id: None,
                name: name.map(str::to_string),
            }
            .into(),
        )
        .unwrap();
        return None;
    }
    Some(result.starters)
}

async fn task_combo_discover(cancel_token: CancellationToken) {
    let tx = global::event_tx();
    let Some(starters) = discover_combo_starters(cancel_token, None).await else {
        return;
    };
    tx.send(ComboEvent::Discovered { starters }.into()).unwrap();
}

fn add_usage(total: &mut UsageStats, delta: &UsageStats) {
    let has_breakdown = delta.input_tokens.is_some() || delta.output_tokens.is_some();
    if has_breakdown {
        let input = total.input_tokens.unwrap_or(0) + delta.input_tokens.unwrap_or(0);
        let output = total.output_tokens.unwrap_or(0) + delta.output_tokens.unwrap_or(0);
        total.input_tokens = Some(input);
        total.output_tokens = Some(output);
        total.total_tokens = Some(input + output);
        return;
    }
    if let Some(delta_total) = delta.total_tokens {
        total.total_tokens = Some(total.total_tokens.unwrap_or(0) + delta_total);
    }
}

fn display_combo_arg(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value.bytes().all(|byte| {
        matches!(byte, b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'_'
            | b'-'
            | b'.'
            | b'/'
            | b':')
    }) {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn is_combo_command_token(token: &str) -> bool {
    if token == "coco" {
        return true;
    }
    let trimmed = trim_combo_token(token);
    std::path::Path::new(&trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "coco")
}

fn trim_combo_token(token: &str) -> String {
    token.trim_matches('"').trim_matches('\'').to_string()
}

fn session_summary_prompt() -> String {
    format!(
        "Write a short session summary for a session switcher list. Return a single line (max {SESSION_SUMMARY_MAX_LEN} chars), plain text only, no quotes. Focus on the user's goal and progress. If there is no meaningful content, return an empty string."
    )
}

async fn generate_session_summary(
    messages: &[ChatMessage],
    model_override: Option<String>,
    thinking_enabled: bool,
) -> Option<String> {
    if messages.is_empty() {
        return None;
    }

    let config = global::config().await;
    let config_dir = config.config_dir.clone();
    let workspace_dir = global::workspace_dir().to_path_buf();
    let mut summary_agent = Agent::new(config);
    summary_agent
        .setup_system_prompt_async(&config_dir, &workspace_dir)
        .await;
    summary_agent.apply_tool_policies(Some(&[]), None);
    summary_agent.set_model_override(model_override);
    summary_agent.set_thinking_enabled(thinking_enabled);
    summary_agent.restore_messages(messages).await;

    let response = match summary_agent
        .chat(ChatMessage::user(ChatContent::Text(
            session_summary_prompt(),
        )))
        .await
    {
        Ok(response) => response,
        Err(err) => {
            warn!(?err, "failed to generate session summary");
            return None;
        }
    };
    let raw = extract_text_response(&response.message);
    sanitize_session_summary(&raw)
}

fn extract_text_response(message: &ChatMessage) -> String {
    match &message.content {
        ChatContent::Text(text) => text.clone(),
        ChatContent::Multiple(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if let ChatBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn sanitize_session_summary(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    let summary: String = trimmed.chars().take(SESSION_SUMMARY_MAX_LEN).collect();
    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

const TOOL_RESULT_MAX_BYTES: usize = 80 * 1024;
const TOOL_RESULT_TRUNCATION_SUFFIX: &str = "\n... (truncated)";

fn combo_run_result_from_final(run_id: &str, output: &Final, is_error: bool) -> ComboRunResult {
    match output {
        Final::Json(value) => combo_run_result_from_json(run_id, value, is_error),
        Final::Message(message) => ComboRunResult {
            run_id: run_id.to_string(),
            success: !is_error,
            summary: message.clone(),
            tool_calls: 0,
            error: if is_error {
                Some(message.clone())
            } else {
                None
            },
        },
    }
}

fn combo_run_result_from_json(run_id: &str, value: &Value, is_error: bool) -> ComboRunResult {
    if let Ok(parsed) = serde_json::from_value::<RunComboOutput>(value.clone()) {
        let error = if is_error && parsed.error.is_none() {
            Some(parsed.summary.clone())
        } else {
            parsed.error.clone()
        };
        return ComboRunResult {
            run_id: run_id.to_string(),
            success: parsed.success,
            summary: parsed.summary,
            tool_calls: parsed.tool_calls,
            error,
        };
    }

    let success = value
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(!is_error);
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_calls = value.get("tool_calls").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut error = value
        .get("error")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    if is_error && error.is_none() {
        error = Some(summary.clone());
    }

    ComboRunResult {
        run_id: run_id.to_string(),
        success,
        summary,
        tool_calls,
        error,
    }
}

fn final_to_tool_content(output: &Final) -> ChatContent {
    let text = match output {
        Final::Json(value) => truncate_json_tool_output(value, TOOL_RESULT_MAX_BYTES)
            .unwrap_or_else(|| {
                let raw = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
                truncate_with_suffix(&raw, TOOL_RESULT_MAX_BYTES, TOOL_RESULT_TRUNCATION_SUFFIX)
            }),
        Final::Message(message) => truncate_with_suffix(
            message,
            TOOL_RESULT_MAX_BYTES,
            TOOL_RESULT_TRUNCATION_SUFFIX,
        ),
    };
    ChatContent::Text(text)
}

fn truncate_json_tool_output(value: &Value, max_bytes: usize) -> Option<String> {
    let obj = value.as_object()?;
    let stdout_value = obj.get("stdout").and_then(|value| value.as_str());
    let stderr_value = obj.get("stderr").and_then(|value| value.as_str());
    if stdout_value.is_none() && stderr_value.is_none() {
        return None;
    }

    let serialized = serde_json::to_string(value).ok()?;
    if serialized.len() <= max_bytes {
        return Some(serialized);
    }

    let stdout = stdout_value.unwrap_or("");
    let stderr = stderr_value.unwrap_or("");
    let stdout_len = stdout.len();
    let stderr_len = stderr.len();

    let mut base = obj.clone();
    if stdout_value.is_some() {
        base.insert("stdout".to_string(), Value::String(String::new()));
    }
    if stderr_value.is_some() {
        base.insert("stderr".to_string(), Value::String(String::new()));
    }
    base.insert("_truncated".to_string(), Value::Bool(true));
    let base_text = serde_json::to_string(&Value::Object(base)).ok()?;
    if base_text.len() >= max_bytes {
        return Some(truncate_with_suffix(
            &base_text,
            max_bytes,
            TOOL_RESULT_TRUNCATION_SUFFIX,
        ));
    }

    let available = max_bytes - base_text.len();
    let total_len = stdout_len + stderr_len;
    let (mut stdout_budget, mut stderr_budget) = if total_len == 0 {
        (0, 0)
    } else if stderr_len == 0 {
        (available, 0)
    } else if stdout_len == 0 {
        (0, available)
    } else {
        let stdout_budget = available * stdout_len / total_len;
        let stderr_budget = available.saturating_sub(stdout_budget);
        (stdout_budget, stderr_budget)
    };

    let mut last_text = base_text;
    for _ in 0..5 {
        let mut out = obj.clone();
        if stdout_value.is_some() {
            let truncated = truncate_to_boundary(stdout, stdout_budget);
            out.insert("stdout".to_string(), Value::String(truncated.to_string()));
        }
        if stderr_value.is_some() {
            let truncated = truncate_to_boundary(stderr, stderr_budget);
            out.insert("stderr".to_string(), Value::String(truncated.to_string()));
        }
        out.insert("_truncated".to_string(), Value::Bool(true));
        let text = serde_json::to_string(&Value::Object(out)).ok()?;
        if text.len() <= max_bytes {
            return Some(text);
        }

        last_text = text;
        if stdout_budget == 0 && stderr_budget == 0 {
            break;
        }
        let overshoot = last_text.len().saturating_sub(max_bytes);
        if stdout_budget >= stderr_budget {
            stdout_budget = stdout_budget.saturating_sub(overshoot);
        } else {
            stderr_budget = stderr_budget.saturating_sub(overshoot);
        }
    }

    Some(truncate_with_suffix(
        &last_text,
        max_bytes,
        TOOL_RESULT_TRUNCATION_SUFFIX,
    ))
}

fn truncate_with_suffix(text: &str, max_bytes: usize, suffix: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }

    let suffix = if max_bytes <= suffix.len() {
        truncate_to_boundary(suffix, max_bytes)
    } else {
        suffix
    };
    if max_bytes <= suffix.len() {
        return suffix.to_string();
    }

    let keep_len = max_bytes - suffix.len();
    let prefix = truncate_to_boundary(text, keep_len);
    let mut out = String::with_capacity(max_bytes);
    out.push_str(prefix);
    out.push_str(suffix);
    out
}

fn truncate_to_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn task_chat(mut agent: Agent, content: ChatContent, cancel_token: CancellationToken) {
    let tx = global::event_tx();

    if cancel_token.is_cancelled() {
        return;
    }
    let msg = ChatMessage::user(content);
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();
    tx.send(AnswerEvent::BotStreamReset.into()).ok();

    let disable_stream = agent.disable_stream_for_current_model();
    let mut streamed_plain = false;
    let mut streamed_thinking = false;
    let chat_resp = if disable_stream {
        agent.chat(msg).await
    } else {
        let stream_tx = tx.clone();
        let plain_seen = Arc::new(AtomicBool::new(false));
        let thinking_seen = Arc::new(AtomicBool::new(false));
        let plain_seen_stream = plain_seen.clone();
        let thinking_seen_stream = thinking_seen.clone();
        let resp = agent
            .chat_stream(msg, cancel_token.clone(), move |update| {
                match update {
                    ChatStreamUpdate::Reset => {
                        plain_seen_stream.store(false, Ordering::Relaxed);
                        thinking_seen_stream.store(false, Ordering::Relaxed);
                        stream_tx.send(AnswerEvent::BotStreamReset.into()).ok();
                    }
                    ChatStreamUpdate::Plain { index, text } => {
                        plain_seen_stream.store(true, Ordering::Relaxed);
                        stream_tx
                            .send(
                                AnswerEvent::BotStream {
                                    index,
                                    kind: BotStreamKind::Plain,
                                    text,
                                }
                                .into(),
                            )
                            .ok();
                    }
                    ChatStreamUpdate::Thinking { index, text } => {
                        thinking_seen_stream.store(true, Ordering::Relaxed);
                        stream_tx
                            .send(
                                AnswerEvent::BotStream {
                                    index,
                                    kind: BotStreamKind::Thinking,
                                    text,
                                }
                                .into(),
                            )
                            .ok();
                    }
                };
            })
            .await;
        streamed_plain = plain_seen.load(Ordering::Relaxed);
        streamed_thinking = thinking_seen.load(Ordering::Relaxed);
        resp
    };

    let chat_resp = match chat_resp {
        Ok(resp) => resp,
        Err(err) => {
            if cancel_token.is_cancelled() {
                tx.send(AnswerEvent::Cancelled.into()).ok();
                return;
            }
            warn!(?err, "chat request failed");
            tx.send(
                AnswerEvent::Bot(vec![BotMessage::System(format!(
                    "Chat request failed: {err}"
                ))])
                .into(),
            )
            .ok();
            return;
        }
    };

    handle_chat_response(
        agent,
        cancel_token,
        chat_resp,
        streamed_plain,
        streamed_thinking,
    )
    .await;
}

async fn task_chat_with_history(mut agent: Agent, cancel_token: CancellationToken) {
    let tx = global::event_tx();

    if cancel_token.is_cancelled() {
        return;
    }
    tx.send(Event::Ask(AskEvent::Bot)).unwrap();
    tx.send(AnswerEvent::BotStreamReset.into()).ok();

    let disable_stream = agent.disable_stream_for_current_model();
    let mut streamed_plain = false;
    let mut streamed_thinking = false;
    let chat_resp = if disable_stream {
        agent.chat_with_history().await
    } else {
        let stream_tx = tx.clone();
        let plain_seen = Arc::new(AtomicBool::new(false));
        let thinking_seen = Arc::new(AtomicBool::new(false));
        let plain_seen_stream = plain_seen.clone();
        let thinking_seen_stream = thinking_seen.clone();
        let resp = agent
            .chat_stream_with_history(cancel_token.clone(), move |update| {
                match update {
                    ChatStreamUpdate::Reset => {
                        plain_seen_stream.store(false, Ordering::Relaxed);
                        thinking_seen_stream.store(false, Ordering::Relaxed);
                        stream_tx.send(AnswerEvent::BotStreamReset.into()).ok();
                    }
                    ChatStreamUpdate::Plain { index, text } => {
                        plain_seen_stream.store(true, Ordering::Relaxed);
                        stream_tx
                            .send(
                                AnswerEvent::BotStream {
                                    index,
                                    kind: BotStreamKind::Plain,
                                    text,
                                }
                                .into(),
                            )
                            .ok();
                    }
                    ChatStreamUpdate::Thinking { index, text } => {
                        thinking_seen_stream.store(true, Ordering::Relaxed);
                        stream_tx
                            .send(
                                AnswerEvent::BotStream {
                                    index,
                                    kind: BotStreamKind::Thinking,
                                    text,
                                }
                                .into(),
                            )
                            .ok();
                    }
                };
            })
            .await;
        streamed_plain = plain_seen.load(Ordering::Relaxed);
        streamed_thinking = thinking_seen.load(Ordering::Relaxed);
        resp
    };

    let chat_resp = match chat_resp {
        Ok(resp) => resp,
        Err(err) => {
            if cancel_token.is_cancelled() {
                tx.send(AnswerEvent::Cancelled.into()).ok();
                return;
            }
            warn!(?err, "chat request failed");
            tx.send(
                AnswerEvent::Bot(vec![BotMessage::System(format!(
                    "Chat request failed: {err}"
                ))])
                .into(),
            )
            .ok();
            return;
        }
    };

    handle_chat_response(
        agent,
        cancel_token,
        chat_resp,
        streamed_plain,
        streamed_thinking,
    )
    .await;
}

async fn handle_chat_response(
    agent: Agent,
    cancel_token: CancellationToken,
    chat_resp: ChatResponse,
    streamed_plain: bool,
    streamed_thinking: bool,
) {
    let tx = global::event_tx();
    if let Some(usage) = chat_resp.usage.clone() {
        tx.send(AnswerEvent::Usage { usage }.into()).ok();
    }
    let mut to_execute: Vec<code_combo::ToolUse> = vec![];
    let mut bot_messages = match chat_resp.message.content {
        ChatContent::Text(text) => {
            if streamed_plain {
                Vec::new()
            } else {
                vec![BotMessage::Plain(text)]
            }
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
                .filter_map(|m| match m {
                    code_combo::Block::Text { text } => {
                        if streamed_plain {
                            None
                        } else {
                            Some(BotMessage::Plain(text))
                        }
                    }
                    code_combo::Block::Thinking { thinking, .. } => {
                        if streamed_thinking {
                            None
                        } else {
                            Some(BotMessage::Thinking(thinking))
                        }
                    }
                    code_combo::Block::ToolUse(tool_use) => Some(BotMessage::ToolUse(tool_use)),
                    code_combo::Block::ToolResult { .. } => None,
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

        // Notify about pending tool executions so results can be batched
        if to_execute.len() > 1 {
            let ids: Vec<String> = to_execute.iter().map(|t| t.id.clone()).collect();
            tx.send(AnswerEvent::PendingToolExecutions { ids }.into())
                .unwrap();
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
                Output::SubagentOutput(event) => {
                    tx.send(
                        AnswerEvent::SubagentEvent {
                            id: id.clone(),
                            event,
                        }
                        .into(),
                    )
                    .unwrap();
                }
                Output::ComboOutput(event) => {
                    tx.send(
                        AnswerEvent::ComboToolEvent {
                            id: id.clone(),
                            event,
                        }
                        .into(),
                    )
                    .unwrap();
                }
            },
        )
        .await;
}

async fn task_combo_execute(
    agent: Agent,
    id: String,
    name: String,
    args: Vec<String>,
    cancel_token: CancellationToken,
) {
    let tx = global::event_tx();
    let auto_accept = agent.auto_accept_edits();
    let id_for_event = id.clone();
    let event_tx = tx.clone();
    let output = agent
        .execute_combo_with_output(name, args, cancel_token.clone(), move |event| {
            let _ = event_tx.send(
                AnswerEvent::ComboToolEvent {
                    id: id_for_event.clone(),
                    event: event.clone(),
                }
                .into(),
            );
        })
        .await;
    match Output::from(output) {
        Output::Success(output) => {
            let _ = tx.send(
                AnswerEvent::ToolResult {
                    id,
                    is_error: false,
                    is_user_cancelled: cancel_token.is_cancelled(),
                    output,
                }
                .into(),
            );
        }
        Output::Failure(output) => {
            let _ = tx.send(
                AnswerEvent::ToolResult {
                    id,
                    is_error: true,
                    is_user_cancelled: cancel_token.is_cancelled(),
                    output,
                }
                .into(),
            );
        }
        Output::TextEdit(edit) => {
            let _ = tx.send(
                AskEvent::TextEdit {
                    id,
                    edit,
                    auto_accept,
                }
                .into(),
            );
        }
        _ => {}
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn truncate_with_suffix_appends_marker() {
        let text = "a".repeat(200);
        let max_bytes = TOOL_RESULT_TRUNCATION_SUFFIX.len() + 10;
        let truncated = truncate_with_suffix(&text, max_bytes, TOOL_RESULT_TRUNCATION_SUFFIX);
        assert!(truncated.len() <= max_bytes);
        assert!(truncated.ends_with(TOOL_RESULT_TRUNCATION_SUFFIX));
    }

    #[test]
    fn truncate_json_tool_output_caps_size() {
        let value = json!({
            "exit_code": 0,
            "stdout": "a".repeat(200),
            "stderr": "",
        });
        let max_bytes = 120;
        let truncated =
            truncate_json_tool_output(&value, max_bytes).expect("expected truncated output");
        assert!(truncated.len() <= max_bytes);
        let parsed: Value = serde_json::from_str(&truncated).expect("truncated output is JSON");
        assert_eq!(
            parsed.get("_truncated").and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn sanitize_session_summary_collapses_whitespace() {
        let raw = "  Hello   world\nsecond   line  ";
        let summary = sanitize_session_summary(raw).expect("expected summary");
        assert_eq!(summary, "Hello world second line");
    }

    #[test]
    fn sanitize_session_summary_truncates() {
        let raw = "a".repeat(SESSION_SUMMARY_MAX_LEN + 20);
        let summary = sanitize_session_summary(&raw).expect("expected summary");
        assert_eq!(summary.len(), SESSION_SUMMARY_MAX_LEN);
    }

    #[test]
    fn sanitize_session_summary_empty_returns_none() {
        assert!(sanitize_session_summary("   \n\t").is_none());
    }
}
