//! Agent module for managing AI assistants.
//!
//! This module provides:
//! - [`Agent`] - The main agent struct for chat interactions
//! - [`AgentConfig`] - Configuration types for customizing agents
//!
//! The module is organized into submodules for better maintainability:
//! - [`streaming`] - Stream event handling and retry logic
//! - [`message`] - Message processing and transformation
//! - [`provider_selection`] - Provider selection and model resolution
//! - [`bash_executor`] - Bash command execution with safety checks
//! - [`config`] - Agent configuration management
//! - [`executor`] - Tool execution framework
//! - [`prompt`] - System prompt building

use std::{sync::Arc, time::Duration};

use crate::provider::{Block, Client, Thinking, ToolChoice};
use futures_util::StreamExt;
use serde_json::json;
use snafu::prelude::*;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    Config, Error, PromptSchema, RequestOptions, Result, ResultDisplayExt, StreamError,
    ThinkingBlocksMode, ThinkingConfig,
    tools::{ComboEvent, ComboInfo, RunComboContext, RunTaskContext, RunTaskTool, run_combo},
};
use executor::PermissionControl;
use message::{
    ensure_thinking_blocks, parse_stringified_tool_input, parse_stringified_tool_inputs_in_message,
    stringify_nested_tool_inputs, stringify_nested_tool_schema, strip_thinking_blocks,
};
use prompt::{build_system_prompt_from_config, build_system_prompt_from_config_async};
use provider_selection::{
    get_current_model, get_resolved_default_model, has_offload_combo_reply, resolve_model,
    select_provider_index,
};

// Re-export from prompt_reply module
pub use crate::prompt_reply::{
    PROMPT_REPLY_TOOL_NAME, PromptReply, REPLY_TOOL_MISSING_ERROR, build_reply_prompt_message,
    build_reply_retry_message, build_reply_tool, build_reply_tool_directive,
    should_use_tool_choice_fallback,
};

// Re-export public types from submodules
pub use crate::provider::{Content, Message, StopReason, ToolUse};
pub use bash_executor::{
    ParsedCommandSummary, bash_unsafe_ranges, bash_unsafe_reason, parse_primary_command,
};
pub use config::{
    AGENT_CONFIG_FILENAME, AgentConfig, AgentConfigError, SubagentConfig, SubagentModelConfig,
    SystemPromptConfig, ToolsConfig, load_agent_config, load_agent_config_for_combo,
    load_agent_config_with_layers,
};
pub use executor::{ExecuteStatus, Executor, Input, Output};

// Internal submodules
mod bash_executor;
mod config;
mod executor;
mod message;
mod prompt;
mod provider_selection;
pub mod streaming;

const DEFAULT_THINKING_BUDGET_TOKENS: usize = 1024;

/// Update type for streaming chat responses.
#[derive(Debug, Clone)]
pub enum ChatStreamUpdate {
    Plain { index: usize, text: String },
    Thinking { index: usize, text: String },
    Reset,
}

/// Response from a chat interaction.
pub struct ChatResponse {
    pub message: Message,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<crate::provider::UsageStats>,
}

/// Import streaming types for internal use.
use streaming::{StreamAccumulator, StreamAction};

/// AI agent for chat interactions and tool execution.
#[derive(Clone)]
pub struct Agent {
    config: Config,
    executor: Executor,

    system_prompt: String,
    /// Shared messages across cloned instances.
    messages: Arc<Mutex<Vec<Message>>>,
    thinking_enabled: bool,
    thinking_cleanup_pending: bool,
    model_override: Option<String>,
    retry_notifier: Option<crate::RetryNotifier>,

    /// Full agent configuration loaded at initialization
    agent_config: AgentConfig,

    /// Shared context for combo execution helpers.
    combo_context: Arc<Mutex<RunComboContext>>,

    /// Shared context for run_task tool (if subagents are configured).
    run_task_context: Option<Arc<Mutex<RunTaskContext>>>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let mut executor = Executor::default();

        let workspace_dir = config
            .workspace_config_path
            .as_deref()
            .and_then(|path| path.parent());

        // Load agent config (builtin -> global -> workspace, always override)
        let agent_config = load_agent_config_with_layers(
            &config.agent_path_layers,
            &config.config_dir,
            workspace_dir.unwrap_or(&config.config_dir),
        )
        .unwrap_or_default();

        // Configure bash safe commands (with agent config as highest priority)
        bash_executor::configure_safe_commands(
            &config.bash_layers,
            &config.config_dir,
            workspace_dir,
            Some(&agent_config),
        );

        // Build system prompt from agent config
        let system_prompt = build_system_prompt_from_config(
            agent_config.system_prompt.as_ref(),
            &config.config_dir,
            workspace_dir.unwrap_or(&config.config_dir),
        );

        // Register run_task tool only if subagents are configured
        // Must be done before apply_tool_policies so it can be retained
        // Note: model_override is initially None, will be updated via set_model_override()
        let run_task_context = if let Some(ref subagents) = agent_config.subagents
            && !subagents.is_empty()
        {
            let context = Arc::new(Mutex::new(RunTaskContext {
                subagents: subagents.clone(),
                config: config.clone(),
                executor: executor.clone(),
                model_override: None,
                default_model: agent_config.default_model.clone(),
            }));
            let run_task_tool = RunTaskTool::new_with_shared_context(context.clone());
            executor.register_tool(std::sync::Arc::new(run_task_tool));
            Some(context)
        } else {
            None
        };

        // Initialize combo context with empty combo list.
        // Combos can be populated later via set_combos().
        let combo_context = Arc::new(Mutex::new(RunComboContext {
            combos: Vec::new(),
            envs: Vec::new(),
            config: config.clone(),
            system_prompt: system_prompt.clone(),
            model_override: None,
            thinking_enabled: false,
            ignore_workspace_scripts: false,
        }));

        // Apply agent tools as base
        if let Some(tools) = agent_config.tools.as_deref() {
            executor.apply_tool_policies(Some(tools), None);
        }

        // Apply config.toml allow/deny on top (existing behavior)
        executor.apply_tool_policies(config.allow_tools.as_deref(), config.deny_tools.as_deref());

        Self {
            config,
            system_prompt,
            executor,
            messages: Arc::new(Mutex::new(vec![])),
            thinking_enabled: false,
            thinking_cleanup_pending: false,
            model_override: None,
            retry_notifier: None,
            agent_config,
            combo_context,
            run_task_context,
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn set_system_prompt(&mut self, system_prompt: &str) {
        self.system_prompt = system_prompt.to_string();
        let system_prompt = self.system_prompt.clone();
        self.update_combo_context(move |ctx| {
            ctx.system_prompt = system_prompt;
        });
    }

    /// Apply tool policies to restrict available tools.
    ///
    /// This is useful for subagents that should only have access to a subset of tools.
    pub fn apply_tool_policies(
        &mut self,
        allow_tools: Option<&[String]>,
        deny_tools: Option<&[String]>,
    ) {
        self.executor.apply_tool_policies(allow_tools, deny_tools);
    }

    /// Get the executor for tool execution.
    ///
    /// This is useful for subagents that need to execute tools directly.
    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    /// Update the combo list for combo execution.
    ///
    /// This should be called after combo discovery to populate the available combos.
    pub async fn set_combos(&self, combos: Vec<ComboInfo>) {
        let mut ctx = self.combo_context.lock().await;
        ctx.combos = combos;
    }

    /// Setup system prompt asynchronously from configuration and AGENTS.md files.
    ///
    /// This method builds the system prompt by:
    /// 1. Loading from agent.toml system_prompt configuration (if present)
    /// 2. Appending global AGENTS.md (if exists)
    /// 3. Appending workspace AGENTS.md (if exists)
    pub async fn setup_system_prompt_async(
        &mut self,
        config_dir: &std::path::Path,
        workspace_dir: &std::path::Path,
    ) {
        let system_prompt = build_system_prompt_from_config_async(
            self.agent_config.system_prompt.as_ref(),
            config_dir,
            workspace_dir,
        )
        .await;

        self.system_prompt = system_prompt.clone();
        let mut ctx = self.combo_context.lock().await;
        ctx.system_prompt = system_prompt;
    }

    /// Get the agent configuration.
    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }

    pub fn name(&self) -> Option<&str> {
        self.agent_config.name.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.agent_config.description.as_deref()
    }

    pub fn default_model(&self) -> Option<&str> {
        self.agent_config.default_model.as_deref()
    }

    pub fn model_override(&self) -> Option<&str> {
        self.model_override.as_deref()
    }

    pub fn set_model_override(&mut self, model: Option<String>) {
        self.model_override = model.clone();
        let model_for_combo = model.clone();
        self.update_combo_context(move |ctx| {
            ctx.model_override = model_for_combo;
        });
        self.update_run_task_context(move |ctx| {
            ctx.model_override = model;
        });
    }

    pub fn set_retry_notifier(&mut self, notifier: Option<crate::RetryNotifier>) {
        self.retry_notifier = notifier;
    }

    pub fn set_ignore_workspace_scripts(&mut self, ignore: bool) {
        self.update_combo_context(move |ctx| {
            ctx.ignore_workspace_scripts = ignore;
        });
    }

    fn update_combo_context<F>(&self, update: F)
    where
        F: FnOnce(&mut RunComboContext) + Send + 'static,
    {
        let ctx = self.combo_context.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut guard = ctx.lock().await;
                update(&mut guard);
            });
        }
    }

    fn update_run_task_context<F>(&self, update: F)
    where
        F: FnOnce(&mut RunTaskContext) + Send + 'static,
    {
        let Some(ctx) = self.run_task_context.clone() else {
            return;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut guard = ctx.lock().await;
                update(&mut guard);
            });
        }
    }

    pub async fn dump_messages(&self) -> Vec<Message> {
        self.messages.lock().await.clone()
    }

    pub async fn append_message(&self, message: Message) {
        self.messages.lock().await.push(message);
    }

    pub async fn restore_messages(&mut self, messages: &[Message]) {
        *self.messages.lock().await = messages.to_vec();
    }

    pub async fn chat(&mut self, message: Message) -> Result<ChatResponse> {
        let request_options = self.request_options_for_current_model();
        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload(&request_options);

        let messages = {
            let mut messages = self.messages.lock().await;
            messages.push(message);
            messages.clone()
        };
        let messages = self.prepare_messages_for_request(messages, &request_options);
        let tools = self.provider_tools_for_request(&request_options);

        let response = client
            .messages(
                Some(&self.system_prompt),
                messages,
                tools,
                thinking,
                &request_options,
            )
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .whatever_context_display("failed to send messages")?;

        let stop_reason = response.stop_reason.clone();
        let usage = response.usage.clone();
        let message = if response.content.is_empty() {
            Message::assistant(Content::Multiple(Vec::default()))
        } else {
            let mut msg = Message::assistant(Content::Multiple(response.content));
            if request_options.stringify_nested_tool_inputs {
                parse_stringified_tool_inputs_in_message(&mut msg, &self.executor);
            }
            self.messages.lock().await.push(msg.clone());
            msg
        };
        self.mark_thinking_cleanup_pending(stop_reason.as_ref());
        Ok(ChatResponse {
            message,
            stop_reason,
            usage,
        })
    }

    pub async fn chat_with_history(&mut self) -> Result<ChatResponse> {
        let request_options = self.request_options_for_current_model();
        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload(&request_options);

        let messages = self.messages.lock().await.clone();
        let messages = self.prepare_messages_for_request(messages, &request_options);
        let tools = self.provider_tools_for_request(&request_options);
        let response = client
            .messages(
                Some(&self.system_prompt),
                messages,
                tools,
                thinking,
                &request_options,
            )
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .whatever_context_display("failed to send messages")?;

        let stop_reason = response.stop_reason.clone();
        let usage = response.usage.clone();
        let message = if response.content.is_empty() {
            Message::assistant(Content::Multiple(Vec::default()))
        } else {
            let mut msg = Message::assistant(Content::Multiple(response.content));
            if request_options.stringify_nested_tool_inputs {
                parse_stringified_tool_inputs_in_message(&mut msg, &self.executor);
            }
            self.messages.lock().await.push(msg.clone());
            msg
        };
        self.mark_thinking_cleanup_pending(stop_reason.as_ref());
        Ok(ChatResponse {
            message,
            stop_reason,
            usage,
        })
    }

    pub async fn chat_stream<F>(
        &mut self,
        message: Message,
        cancel_token: CancellationToken,
        on_update: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(ChatStreamUpdate) + Send,
    {
        let request_options = self.request_options_for_current_model();
        self.chat_stream_internal(Some(message), cancel_token, on_update, &request_options)
            .await
    }

    pub async fn chat_stream_with_history<F>(
        &mut self,
        cancel_token: CancellationToken,
        on_update: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(ChatStreamUpdate) + Send,
    {
        let request_options = self.request_options_for_current_model();
        self.chat_stream_internal(None, cancel_token, on_update, &request_options)
            .await
    }

    async fn chat_stream_internal<F>(
        &mut self,
        message: Option<Message>,
        cancel_token: CancellationToken,
        mut on_update: F,
        request_options: &RequestOptions,
    ) -> Result<ChatResponse>
    where
        F: FnMut(ChatStreamUpdate) + Send,
    {
        use streaming::{
            notify_stream_retry_attempt, notify_stream_retry_finished, stream_retry_delay,
            wait_for_retry,
        };

        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload(request_options);

        let messages = {
            let mut messages = self.messages.lock().await;
            if let Some(message) = message {
                messages.push(message);
            }
            messages.clone()
        };
        let messages = self.prepare_messages_for_request(messages, request_options);
        let max_attempts = request_options.retry_max_attempts;
        let max_delay = Duration::from_millis(request_options.retry_max_delay_ms);
        let mut attempt = 0usize;
        let mut retried = false;

        loop {
            let tools = self.provider_tools_for_request(request_options);
            let stream_result = client
                .messages_stream(
                    Some(&self.system_prompt),
                    messages.clone(),
                    tools,
                    thinking.clone(),
                    request_options,
                )
                .await
                .inspect_err(|err| {
                    warn!("send messsages stream error: {err:?}");
                })
                .whatever_context_display("failed to send messages stream");

            let mut stream = match stream_result {
                Ok(stream) => stream,
                Err(err) => {
                    if retried {
                        notify_stream_retry_finished(request_options, false);
                    }
                    return Err(err);
                }
            };

            let mut accumulator = StreamAccumulator::new();
            let mut stream_error: Option<StreamError> = None;
            loop {
                let event = tokio::select! {
                    _ = cancel_token.cancelled() => {
                        if retried {
                            notify_stream_retry_finished(request_options, false);
                        }
                        whatever!("chat stream cancelled");
                    }
                    event = stream.next() => event,
                };
                let Some(event) = event else {
                    break;
                };
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        stream_error = Some(err.with_context("read messages stream error"));
                        break;
                    }
                };
                let action = match accumulator.handle_event(event, &mut on_update) {
                    Ok(action) => action,
                    Err(err) => {
                        stream_error = Some(StreamError::decode(format!(
                            "parse messages stream error: {err}"
                        )));
                        break;
                    }
                };
                if matches!(action, StreamAction::Stop) {
                    break;
                }
            }

            if let Some(err) = stream_error {
                if err.is_retryable() && attempt < max_attempts {
                    attempt += 1;
                    retried = true;
                    let delay = stream_retry_delay(attempt, max_delay);
                    notify_stream_retry_attempt(request_options, attempt, delay, &err);
                    on_update(ChatStreamUpdate::Reset);
                    if !wait_for_retry(delay, &cancel_token).await {
                        if retried {
                            notify_stream_retry_finished(request_options, false);
                        }
                        whatever!("chat stream cancelled");
                    }
                    continue;
                }
                if retried {
                    notify_stream_retry_finished(request_options, false);
                }
                return Err(Error::stream(err.kind, err.message));
            }

            let (blocks, stop_reason, usage) = accumulator.finish();
            let message = if blocks.is_empty() {
                Message::assistant(Content::Multiple(Vec::default()))
            } else {
                let mut msg = Message::assistant(Content::Multiple(blocks));
                if request_options.stringify_nested_tool_inputs {
                    parse_stringified_tool_inputs_in_message(&mut msg, &self.executor);
                }
                self.messages.lock().await.push(msg.clone());
                msg
            };
            self.mark_thinking_cleanup_pending(stop_reason.as_ref());
            if retried {
                notify_stream_retry_finished(request_options, true);
            }
            return Ok(ChatResponse {
                message,
                stop_reason,
                usage,
            });
        }
    }

    pub async fn reply_prompt(
        &mut self,
        system_prompt: &str,
        schemas: Vec<PromptSchema>,
    ) -> Result<PromptReply> {
        self.reply_prompt_with_thinking(system_prompt, schemas, None)
            .await
    }

    pub async fn reply_prompt_with_thinking(
        &mut self,
        system_prompt: &str,
        schemas: Vec<PromptSchema>,
        thinking: Option<ThinkingConfig>,
    ) -> Result<PromptReply> {
        ensure_whatever!(!schemas.is_empty(), "schemas cannot be empty");
        let request_options = self.request_options_for_current_model();
        let use_tool_choice_fallback = should_use_tool_choice_fallback(&request_options);
        ensure_whatever!(
            !request_options.disable_tools,
            "reply tool disabled by request options"
        );
        let (_, client) = self.pick_provider()?;
        let tool_choice = ToolChoice::tool(PROMPT_REPLY_TOOL_NAME, None);
        let system_prompt = system_prompt.trim();
        let system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };
        let thinking = self.thinking_payload_with_override(&request_options, thinking.as_ref());
        let mut attempt = 0usize;
        loop {
            let reply_tool = build_reply_tool(&schemas)?;
            let messages = {
                let mut history = self.messages.lock().await;
                if attempt == 0 && use_tool_choice_fallback {
                    let new_message = build_reply_prompt_message(&schemas);
                    history.push(new_message);
                } else if attempt > 0 {
                    history.push(build_reply_retry_message(&schemas));
                }
                history.clone()
            };
            let messages = self.prepare_messages_for_request(messages, &request_options);
            let response = if request_options.disable_tool_choice {
                ensure_whatever!(
                    request_options.tool_choice_fallback,
                    "tool_choice disabled without prompt fallback"
                );
                client
                    .messages(
                        system_prompt,
                        messages,
                        vec![reply_tool],
                        thinking.clone(),
                        &request_options,
                    )
                    .await
                    .whatever_context_display("failed to request prompt reply")?
            } else {
                client
                    .messages_with_tool_choice(
                        system_prompt,
                        messages,
                        vec![reply_tool],
                        tool_choice.clone(),
                        thinking.clone(),
                        &request_options,
                    )
                    .await
                    .whatever_context_display("failed to request prompt reply")?
            };
            let stop_reason = response.stop_reason.clone();
            let usage = response.usage.clone();
            if !response.content.is_empty() {
                let mut history = self.messages.lock().await;
                history.push(Message::assistant(Content::Multiple(
                    response.content.clone(),
                )));
            }
            let mut thinking = Vec::new();
            let mut reply_tool_use = None;
            for block in response.content.into_iter() {
                match block {
                    Block::Thinking { thinking: text, .. } => {
                        thinking.push(text);
                    }
                    Block::ToolUse(tool_use) if tool_use.name == PROMPT_REPLY_TOOL_NAME => {
                        reply_tool_use = Some(tool_use);
                    }
                    _ => (),
                }
            }
            let Some(tool_use) = reply_tool_use else {
                if attempt >= request_options.combo_reply_retries {
                    whatever!("{}", REPLY_TOOL_MISSING_ERROR);
                }
                attempt += 1;
                continue;
            };
            {
                let mut history = self.messages.lock().await;
                history.push(Message::user(Content::Multiple(vec![Block::tool_result(
                    &tool_use.id,
                    None,
                    Content::Text("ok".to_string()),
                )])));
            }
            let response = serde_json::to_string(&tool_use.input)
                .whatever_context("failed to serialize reply tool input")?;
            self.mark_thinking_cleanup_pending(stop_reason.as_ref());
            return Ok(PromptReply {
                tool_use,
                response,
                thinking,
                usage,
            });
        }
    }

    pub async fn reply_prompt_stream_with_thinking<F>(
        &mut self,
        system_prompt: &str,
        schemas: Vec<PromptSchema>,
        thinking: Option<ThinkingConfig>,
        cancel_token: CancellationToken,
        mut on_update: F,
    ) -> Result<PromptReply>
    where
        F: FnMut(ChatStreamUpdate) + Send,
    {
        ensure_whatever!(!schemas.is_empty(), "schemas cannot be empty");
        let request_options = self.request_options_for_current_model();
        let use_tool_choice_fallback = should_use_tool_choice_fallback(&request_options);
        ensure_whatever!(
            !request_options.disable_tools,
            "reply tool disabled by request options"
        );
        let (_, client) = self.pick_provider()?;
        let tool_choice = ToolChoice::tool(PROMPT_REPLY_TOOL_NAME, None);
        let system_prompt = system_prompt.trim();
        let system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };
        let thinking = self.thinking_payload_with_override(&request_options, thinking.as_ref());
        let mut attempt = 0usize;
        loop {
            let reply_tool = build_reply_tool(&schemas)?;
            let messages = {
                let mut history = self.messages.lock().await;
                if attempt == 0 && use_tool_choice_fallback {
                    let new_message = build_reply_prompt_message(&schemas);
                    history.push(new_message);
                } else if attempt > 0 {
                    history.push(build_reply_retry_message(&schemas));
                }
                history.clone()
            };
            let messages = self.prepare_messages_for_request(messages, &request_options);
            let mut stream = if request_options.disable_tool_choice {
                ensure_whatever!(
                    request_options.tool_choice_fallback,
                    "tool_choice disabled without prompt fallback"
                );
                client
                    .messages_stream(
                        system_prompt,
                        messages,
                        vec![reply_tool],
                        thinking.clone(),
                        &request_options,
                    )
                    .await
                    .inspect_err(|err| {
                        warn!("send prompt reply stream error: {err:?}");
                    })
                    .whatever_context_display("failed to request prompt reply stream")?
            } else {
                client
                    .messages_stream_with_tool_choice(
                        system_prompt,
                        messages,
                        vec![reply_tool],
                        tool_choice.clone(),
                        thinking.clone(),
                        &request_options,
                    )
                    .await
                    .inspect_err(|err| {
                        warn!("send prompt reply stream error: {err:?}");
                    })
                    .whatever_context_display("failed to request prompt reply stream")?
            };

            let mut accumulator = StreamAccumulator::new();
            while let Some(event) = tokio::select! {
                _ = cancel_token.cancelled() => {
                    whatever!("prompt reply stream cancelled");
                }
                event = stream.next() => event,
            } {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        let err = err.with_context("read prompt reply stream error");
                        return Err(Error::stream(err.kind, err.message));
                    }
                };
                let action = accumulator
                    .handle_event(event, &mut on_update)
                    .whatever_context("parse prompt reply stream error")?;
                if matches!(action, StreamAction::Stop) {
                    break;
                }
            }

            let (blocks, stop_reason, usage) = accumulator.finish();
            if !blocks.is_empty() {
                let msg = Message::assistant(Content::Multiple(blocks.clone()));
                self.messages.lock().await.push(msg);
            }
            let mut thinking = Vec::new();
            let mut reply_tool_use = None;
            for block in &blocks {
                match block {
                    Block::Thinking { thinking: text, .. } => {
                        thinking.push(text.clone());
                    }
                    Block::ToolUse(tool_use) if tool_use.name == PROMPT_REPLY_TOOL_NAME => {
                        reply_tool_use = Some(tool_use.clone());
                    }
                    _ => (),
                }
            }
            let Some(tool_use) = reply_tool_use else {
                if attempt >= request_options.combo_reply_retries {
                    whatever!("{}", REPLY_TOOL_MISSING_ERROR);
                }
                attempt += 1;
                continue;
            };
            {
                let mut history = self.messages.lock().await;
                history.push(Message::user(Content::Multiple(vec![Block::tool_result(
                    &tool_use.id,
                    None,
                    Content::Text("ok".to_string()),
                )])));
            }
            let response = serde_json::to_string(&tool_use.input)
                .whatever_context("failed to serialize reply tool input")?;
            self.mark_thinking_cleanup_pending(stop_reason.as_ref());
            return Ok(PromptReply {
                tool_use,
                response,
                thinking,
                usage,
            });
        }
    }

    pub fn grant_once(&mut self, id: &str, name: &str) {
        self.executor
            .update_pcl(name, PermissionControl::Once(id.to_string()))
    }

    pub fn grant_session(&mut self, tool_use: &ToolUse) {
        self.executor.grant_session(&tool_use.name, &tool_use.input)
    }

    pub fn set_auto_accept_edits(&mut self, enabled: bool) {
        self.executor.set_auto_accept_edits(enabled);
    }

    pub fn auto_accept_edits(&self) -> bool {
        self.executor.auto_accept_edits()
    }

    /// Set environment variables to inject when executing bash commands.
    pub fn set_bash_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.executor.set_bash_env(key, value);
    }

    /// Remove an environment variable from bash command injection.
    pub fn remove_bash_env(&mut self, key: &str) {
        self.executor.remove_bash_env(key);
    }

    pub fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
        self.update_combo_context(move |ctx| {
            ctx.thinking_enabled = enabled;
        });
    }

    pub fn thinking_enabled(&self) -> bool {
        self.thinking_enabled
    }

    pub fn disable_stream_for_current_model(&self) -> bool {
        self.request_options_for_current_model().disable_stream
    }

    pub fn combo_reply_retries(&self) -> usize {
        self.request_options_for_current_model().combo_reply_retries
    }

    pub fn context_window(&self) -> Option<usize> {
        self.request_options_for_current_model().context_window
    }

    pub fn current_model(&self) -> String {
        get_current_model(
            self.default_model(),
            self.model_override.as_deref(),
            &self.config.providers,
        )
    }

    pub fn resolved_default_model(&self) -> String {
        get_resolved_default_model(self.default_model(), &self.config.providers)
    }

    /// Check if the current provider has offload_combo_reply enabled.
    pub fn offload_combo_reply(&self) -> bool {
        has_offload_combo_reply(
            self.default_model(),
            self.model_override.as_deref(),
            &self.config.providers,
            &self.config,
        )
    }

    fn selected_model(&self) -> Option<String> {
        self.model_override
            .clone()
            .or_else(|| self.default_model().map(|s| s.to_string()))
    }

    pub async fn execute<'a>(
        &mut self,
        id: &str,
        name: &str,
        input: executor::Input<'a>,
    ) -> Output {
        let input = self.normalize_tool_input_for_execution(name, input);
        self.executor
            .execute(id, name, input)
            .await
            .expect("Failed to execute")
    }

    pub async fn execute_with_output<'a, F>(
        &mut self,
        id: &str,
        name: &str,
        input: executor::Input<'a>,
        cancel_token: CancellationToken,
        on_output: F,
    ) -> Result<ExecuteStatus>
    where
        F: FnMut(Output) + Send,
    {
        let input = self.normalize_tool_input_for_execution(name, input);
        self.executor
            .execute_with_output(id, name, input, cancel_token, on_output)
            .await
    }

    pub async fn execute_combo_with_output<F>(
        &self,
        name: String,
        args: Vec<String>,
        cancel_token: CancellationToken,
        on_event: F,
    ) -> crate::tools::ExecuteResult
    where
        F: FnMut(&ComboEvent) + Send + 'static,
    {
        let input = json!({
            "combo_name": name,
            "args": args,
        });
        run_combo(
            self.combo_context.clone(),
            crate::tools::Input::Starter(input),
            cancel_token,
            on_event,
        )
        .await
    }

    fn thinking_payload(&self, request_options: &RequestOptions) -> Option<Thinking> {
        if self.thinking_enabled {
            Some(Thinking::enabled(
                request_options
                    .thinking_budget_tokens
                    .unwrap_or(DEFAULT_THINKING_BUDGET_TOKENS),
            ))
        } else {
            None
        }
    }

    fn thinking_payload_with_override(
        &self,
        request_options: &RequestOptions,
        thinking: Option<&ThinkingConfig>,
    ) -> Option<Thinking> {
        let Some(thinking) = thinking else {
            return self.thinking_payload(request_options);
        };
        if !thinking.enabled {
            return None;
        }
        let budget_tokens = thinking.budget_tokens.unwrap_or(
            request_options
                .thinking_budget_tokens
                .unwrap_or(DEFAULT_THINKING_BUDGET_TOKENS),
        );
        Some(Thinking::enabled(budget_tokens))
    }

    fn request_options_for_current_model(&self) -> RequestOptions {
        let selected_model = self.selected_model();
        match select_provider_index(selected_model.as_deref(), &self.config.providers) {
            Ok(idx) => {
                let provider = &self.config.providers[idx];
                let model = resolve_model(provider, selected_model);
                let mut options = self.config.request_options_for_model(&model);
                options.apply_override(&provider.request_overrides);
                options.retry_notifier = self.retry_notifier.clone();
                options
            }
            Err(_) => RequestOptions::default(),
        }
    }

    fn normalize_tool_input_for_execution<'a>(
        &self,
        name: &str,
        input: executor::Input<'a>,
    ) -> executor::Input<'a> {
        let request_options = self.request_options_for_current_model();
        if !request_options.stringify_nested_tool_inputs {
            return input;
        }
        match input {
            Input::Starter(value) => {
                let schema = self.executor.tool_input_schema(name);
                let value = match schema.as_ref() {
                    Some(schema) => parse_stringified_tool_input(value, schema),
                    None => value,
                };
                Input::Starter(value)
            }
            input => input,
        }
    }

    fn provider_tools_for_request(
        &self,
        request_options: &RequestOptions,
    ) -> Vec<crate::provider::Tool> {
        if request_options.disable_tools {
            Vec::new()
        } else {
            let mut tools = self.executor.provider_tools();
            if request_options.stringify_nested_tool_inputs {
                for tool in &mut tools {
                    tool.input_schema = stringify_nested_tool_schema(&tool.input_schema);
                }
            }
            tools
        }
    }

    fn prepare_messages_for_request(
        &mut self,
        messages: Vec<Message>,
        request_options: &RequestOptions,
    ) -> Vec<Message> {
        let mut messages = match request_options.thinking_blocks {
            ThinkingBlocksMode::DropAfterTurn => {
                if self.thinking_cleanup_pending {
                    self.thinking_cleanup_pending = false;
                    strip_thinking_blocks(&messages)
                } else {
                    messages
                }
            }
            ThinkingBlocksMode::Keep => messages,
            ThinkingBlocksMode::DropAlways => {
                if self.thinking_cleanup_pending {
                    self.thinking_cleanup_pending = false;
                }
                strip_thinking_blocks(&messages)
            }
        };
        if request_options.ensure_toolcall_thinking
            && !matches!(
                request_options.thinking_blocks,
                ThinkingBlocksMode::DropAlways
            )
        {
            ensure_thinking_blocks(&mut messages);
        }
        if request_options.stringify_nested_tool_inputs {
            messages = stringify_nested_tool_inputs(messages, &self.executor);
        }
        messages
    }

    fn mark_thinking_cleanup_pending(&mut self, reason: Option<&StopReason>) {
        if matches!(reason, Some(StopReason::EndTurn)) {
            self.thinking_cleanup_pending = true;
        }
    }

    fn pick_provider(&mut self) -> Result<(&str, Client)> {
        let selected_model = self.selected_model();

        let provider_idx =
            select_provider_index(selected_model.as_deref(), &self.config.providers)?;
        let provider = &mut self.config.providers[provider_idx];

        // Use model override or agent's default_model if configured,
        // otherwise fallback to first provider.models,
        // otherwise fallback to provider.name
        let model = resolve_model(provider, selected_model);

        let client = Client::new(provider, &model, crate::version::user_agent().to_string())?;
        Ok((&provider.name, client))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnvString, ModelRequestConfig, ProviderConfig, ProviderKind};

    #[test]
    fn request_options_provider_override_stringify_nested_tool_inputs() {
        let mut config = Config::default();
        config.providers.push(ProviderConfig {
            name: "demo".to_string(),
            kind: ProviderKind::OpenAI,
            api_key: EnvString::String("test".to_string()),
            base_url: "http://localhost".to_string(),
            models: None,
            request_overrides: ModelRequestConfig {
                stringify_nested_tool_inputs: Some(true),
                ..ModelRequestConfig::default()
            },
        });
        let agent = Agent::new(config);
        let options = agent.request_options_for_current_model();
        assert!(options.stringify_nested_tool_inputs);
    }

    #[test]
    fn prepare_messages_inserts_thinking_for_tool_use() {
        let mut agent = Agent::new(Config::default());
        let options = RequestOptions {
            ensure_toolcall_thinking: true,
            ..Default::default()
        };
        let messages = vec![
            Message::assistant(Content::Multiple(vec![Block::ToolUse(ToolUse {
                id: "tool_1".to_string(),
                name: "combo_reply".to_string(),
                input: serde_json::Value::Null,
            })])),
            Message::assistant(Content::Multiple(vec![
                Block::Thinking {
                    thinking: "Reasoning".to_string(),
                    signature: None,
                },
                Block::ToolUse(ToolUse {
                    id: "tool_2".to_string(),
                    name: "combo_reply".to_string(),
                    input: serde_json::Value::Null,
                }),
            ])),
        ];

        let prepared = agent.prepare_messages_for_request(messages, &options);

        match &prepared[0].content {
            Content::Multiple(blocks) => assert!(matches!(blocks[0], Block::Thinking { .. })),
            _ => panic!("expected multiple blocks"),
        }
        match &prepared[1].content {
            Content::Multiple(blocks) => {
                let thinking_count = blocks
                    .iter()
                    .filter(|block| matches!(block, Block::Thinking { .. }))
                    .count();
                assert_eq!(thinking_count, 1);
            }
            _ => panic!("expected multiple blocks"),
        }
    }

    #[test]
    fn prepare_messages_strip_all_removes_thinking() {
        let mut agent = Agent::new(Config::default());
        let options = RequestOptions {
            thinking_blocks: ThinkingBlocksMode::DropAlways,
            ensure_toolcall_thinking: true,
            ..Default::default()
        };
        let messages = vec![Message::assistant(Content::Multiple(vec![
            Block::Thinking {
                thinking: "Reasoning".to_string(),
                signature: None,
            },
            Block::ToolUse(ToolUse {
                id: "tool_1".to_string(),
                name: "combo_reply".to_string(),
                input: serde_json::Value::Null,
            }),
        ]))];

        let prepared = agent.prepare_messages_for_request(messages, &options);

        match &prepared[0].content {
            Content::Multiple(blocks) => {
                assert!(
                    blocks
                        .iter()
                        .all(|block| !matches!(block, Block::Thinking { .. }))
                );
            }
            _ => panic!("expected multiple blocks"),
        }
    }
}
