//! Agent module for managing AI assistants.
//!
//! This module provides:
//! - [`Agent`] - The main agent struct for chat interactions
//! - [`AgentConfig`] - Configuration types for customizing agents

use std::{collections::HashMap, sync::Arc};

use crate::provider::{
    Block, Client, ContentBlockDelta, MessagesStreamEvent, Thinking, ToolChoice,
};
use futures_util::StreamExt;
use serde_json::{Map as JsonMap, json};
use snafu::prelude::*;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    Config, PromptSchema, ProviderConfig, RequestOptions, Result, ResultDisplayExt, ThinkingConfig,
    tools::{RunTaskContext, RunTaskTool},
};
use executor::PermissionControl;
use prompt::{build_system_prompt_from_config, build_system_prompt_from_config_async};

mod bash_executor;
mod config;
mod executor;
mod prompt;

pub use crate::provider::{Content, Message, StopReason, ToolUse};
pub use bash_executor::{bash_unsafe_ranges, bash_unsafe_reason};
pub use executor::{ExecuteStatus, Executor, Input, Output};

const DEFAULT_THINKING_BUDGET_TOKENS: usize = 1024;
pub use config::{
    AGENT_CONFIG_FILENAME, AgentConfig, AgentConfigError, SubagentConfig, SystemPromptConfig,
    ToolsConfig, load_agent_config, load_agent_config_for_combo, load_agent_config_with_layers,
};

const PROMPT_REPLY_TOOL_NAME: &str = "combo_reply";

#[derive(Clone)]
pub struct Agent {
    config: Config,
    executor: Executor,

    system_prompt: String,
    /// Shared messages across cloned instances.
    messages: Arc<Mutex<Vec<Message>>>,
    thinking_enabled: bool,
    thinking_budget_tokens: usize,
    thinking_cleanup_pending: bool,
    model_override: Option<String>,

    /// Full agent configuration loaded at initialization
    agent_config: AgentConfig,
}

pub struct ChatResponse {
    pub message: Message,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone)]
pub enum ChatStreamUpdate {
    Plain { index: usize, text: String },
    Thinking { index: usize, text: String },
}

pub struct PromptReply {
    pub tool_use: ToolUse,
    pub response: String,
    pub thinking: Vec<String>,
}

enum StreamAction {
    Continue,
    Stop,
}

struct StreamAccumulator {
    blocks: Vec<Block>,
    tool_inputs: HashMap<usize, String>,
    stop_reason: Option<StopReason>,
}

impl StreamAccumulator {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            tool_inputs: HashMap::new(),
            stop_reason: None,
        }
    }

    fn finish(self) -> (Vec<Block>, Option<StopReason>) {
        (self.blocks, self.stop_reason)
    }

    fn handle_event<F>(
        &mut self,
        event: MessagesStreamEvent,
        on_update: &mut F,
    ) -> Result<StreamAction>
    where
        F: FnMut(ChatStreamUpdate),
    {
        match event {
            MessagesStreamEvent::MessageStart { .. } => Ok(StreamAction::Continue),
            MessagesStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                self.ensure_block_slot(index);
                self.blocks[index] = content_block;
                match &self.blocks[index] {
                    Block::Text { text } if !text.is_empty() => {
                        on_update(ChatStreamUpdate::Plain {
                            index,
                            text: text.clone(),
                        });
                    }
                    Block::Thinking { thinking, .. } if !thinking.is_empty() => {
                        on_update(ChatStreamUpdate::Thinking {
                            index,
                            text: thinking.clone(),
                        });
                    }
                    _ => (),
                }
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::ContentBlockDelta { index, delta } => {
                self.apply_delta(index, delta, on_update)?;
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::ContentBlockStop { index } => {
                self.finalize_tool_input(index)?;
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::MessageDelta { delta, .. } => {
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::MessageStop => Ok(StreamAction::Stop),
            MessagesStreamEvent::Ping => Ok(StreamAction::Continue),
            MessagesStreamEvent::Error { error } => {
                let mut message = format!("stream error: {}", error.message);
                if let Some(code) = error.code.as_deref() {
                    message.push_str(&format!(" (code: {code})"));
                }
                if let Some(kind) = error.r#type.as_deref() {
                    message.push_str(&format!(" (type: {kind})"));
                }
                whatever!("{message}")
            }
            MessagesStreamEvent::Unknown { .. } => Ok(StreamAction::Continue),
        }
    }

    fn ensure_block_slot(&mut self, index: usize) {
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, || Block::Text {
                text: String::new(),
            });
        }
    }

    fn apply_delta<F>(
        &mut self,
        index: usize,
        delta: ContentBlockDelta,
        on_update: &mut F,
    ) -> Result<()>
    where
        F: FnMut(ChatStreamUpdate),
    {
        self.ensure_block_slot(index);
        match delta {
            ContentBlockDelta::TextDelta { text } => {
                if text.is_empty() {
                    return Ok(());
                }
                match &mut self.blocks[index] {
                    Block::Text { text: current } => current.push_str(&text),
                    _ => {
                        self.blocks[index] = Block::Text { text: text.clone() };
                    }
                }
                on_update(ChatStreamUpdate::Plain { index, text });
            }
            ContentBlockDelta::ThinkingDelta { thinking } => {
                if thinking.is_empty() {
                    return Ok(());
                }
                match &mut self.blocks[index] {
                    Block::Thinking {
                        thinking: current, ..
                    } => current.push_str(&thinking),
                    _ => {
                        self.blocks[index] = Block::Thinking {
                            thinking: thinking.clone(),
                            signature: None,
                        };
                    }
                }
                on_update(ChatStreamUpdate::Thinking {
                    index,
                    text: thinking,
                });
            }
            ContentBlockDelta::SignatureDelta { signature } => {
                if let Block::Thinking {
                    signature: slot, ..
                } = &mut self.blocks[index]
                {
                    *slot = Some(signature);
                }
            }
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                if !partial_json.is_empty() {
                    self.tool_inputs
                        .entry(index)
                        .or_default()
                        .push_str(&partial_json);
                }
            }
            ContentBlockDelta::Unknown => (),
        }
        Ok(())
    }

    fn finalize_tool_input(&mut self, index: usize) -> Result<()> {
        let Some(buffer) = self.tool_inputs.remove(&index) else {
            return Ok(());
        };
        if buffer.is_empty() {
            return Ok(());
        }
        let input: serde_json::Value =
            serde_json::from_str(&buffer).whatever_context("decode tool input json")?;
        if let Some(Block::ToolUse(tool_use)) = self.blocks.get_mut(index) {
            tool_use.input = input;
        }
        Ok(())
    }
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let mut executor = Executor::default();

        let workspace_dir = config
            .workspace_config_path
            .as_deref()
            .and_then(|path| path.parent());

        let thinking_budget_tokens = config
            .providers
            .first()
            .and_then(|provider| provider.thinking_budget_tokens)
            .unwrap_or(DEFAULT_THINKING_BUDGET_TOKENS);

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

        // Register run_task tool only if subagents are configured
        // Must be done before apply_tool_policies so it can be retained
        if let Some(ref subagents) = agent_config.subagents
            && !subagents.is_empty()
        {
            let run_task_context = RunTaskContext {
                subagents: subagents.clone(),
                config: config.clone(),
                executor: executor.clone(),
            };
            let run_task_tool = RunTaskTool::new(run_task_context);
            executor.register_tool(std::sync::Arc::new(run_task_tool));
        }

        // Apply agent tools as base
        if let Some(tools) = agent_config.tools.as_deref() {
            executor.apply_tool_policies(Some(tools), None);
        }

        // Apply config.toml allow/deny on top (existing behavior)
        executor.apply_tool_policies(config.allow_tools.as_deref(), config.deny_tools.as_deref());

        // Build system prompt from agent config
        let system_prompt = build_system_prompt_from_config(
            agent_config.system_prompt.as_ref(),
            &config.config_dir,
            workspace_dir.unwrap_or(&config.config_dir),
        );

        Self {
            config,
            system_prompt,
            executor,
            messages: Arc::new(Mutex::new(vec![])),
            thinking_enabled: false,
            thinking_budget_tokens,
            thinking_cleanup_pending: false,
            model_override: None,
            agent_config,
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn set_system_prompt(&mut self, system_prompt: &str) {
        self.system_prompt = system_prompt.to_string()
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

        self.system_prompt = system_prompt;
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
        self.model_override = model;
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
        let thinking = self.thinking_payload();

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
        let message = if response.content.is_empty() {
            Message::assistant(Content::Multiple(Vec::default()))
        } else {
            let msg = Message::assistant(Content::Multiple(response.content));
            self.messages.lock().await.push(msg.clone());
            msg
        };
        self.mark_thinking_cleanup_pending(stop_reason.as_ref());
        Ok(ChatResponse {
            message,
            stop_reason,
        })
    }

    pub async fn chat_with_history(&mut self) -> Result<ChatResponse> {
        let request_options = self.request_options_for_current_model();
        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload();

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
        let message = if response.content.is_empty() {
            Message::assistant(Content::Multiple(Vec::default()))
        } else {
            let msg = Message::assistant(Content::Multiple(response.content));
            self.messages.lock().await.push(msg.clone());
            msg
        };
        self.mark_thinking_cleanup_pending(stop_reason.as_ref());
        Ok(ChatResponse {
            message,
            stop_reason,
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
        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload();

        let messages = {
            let mut messages = self.messages.lock().await;
            if let Some(message) = message {
                messages.push(message);
            }
            messages.clone()
        };
        let messages = self.prepare_messages_for_request(messages, request_options);
        let tools = self.provider_tools_for_request(request_options);

        let mut stream = client
            .messages_stream(
                Some(&self.system_prompt),
                messages,
                tools,
                thinking,
                request_options,
            )
            .await
            .inspect_err(|err| {
                warn!("send messsages stream error: {err:?}");
            })
            .whatever_context_display("failed to send messages stream")?;

        let mut accumulator = StreamAccumulator::new();
        while let Some(event) = tokio::select! {
            _ = cancel_token.cancelled() => {
                whatever!("chat stream cancelled");
            }
            event = stream.next() => event,
        } {
            let event = event.whatever_context_display("read messages stream error")?;
            let action = accumulator
                .handle_event(event, &mut on_update)
                .whatever_context("parse messages stream error")?;
            if matches!(action, StreamAction::Stop) {
                break;
            }
        }

        let (blocks, stop_reason) = accumulator.finish();
        let message = if blocks.is_empty() {
            Message::assistant(Content::Multiple(Vec::default()))
        } else {
            let msg = Message::assistant(Content::Multiple(blocks));
            self.messages.lock().await.push(msg.clone());
            msg
        };
        self.mark_thinking_cleanup_pending(stop_reason.as_ref());
        Ok(ChatResponse {
            message,
            stop_reason,
        })
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
        let reply_tool = build_reply_tool(&schemas)?;
        let (_, client) = self.pick_provider()?;
        let messages = {
            let mut history = self.messages.lock().await;
            if use_tool_choice_fallback {
                let new_message = build_reply_prompt_message(&schemas);
                history.push(new_message);
            }
            history.clone()
        };
        let messages = self.prepare_messages_for_request(messages, &request_options);
        let tool_choice = ToolChoice::tool(PROMPT_REPLY_TOOL_NAME, None);
        let system_prompt = system_prompt.trim();
        let system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };
        let thinking = self.thinking_payload_with_override(thinking.as_ref());
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
                    thinking,
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
                    tool_choice,
                    thinking,
                    &request_options,
                )
                .await
                .whatever_context_display("failed to request prompt reply")?
        };
        let stop_reason = response.stop_reason.clone();
        if !response.content.is_empty() {
            let mut history = self.messages.lock().await;
            history.push(Message::assistant(Content::Multiple(
                response.content.clone(),
            )));
        }
        let mut thinking = Vec::new();
        let mut reply_tool = None;
        for block in response.content.into_iter() {
            match block {
                Block::Thinking { thinking: text, .. } => {
                    thinking.push(text);
                }
                Block::ToolUse(tool_use) if tool_use.name == PROMPT_REPLY_TOOL_NAME => {
                    reply_tool = Some(tool_use);
                }
                _ => (),
            }
        }
        let Some(tool_use) = reply_tool else {
            whatever!("reply tool use not found in response");
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
        Ok(PromptReply {
            tool_use,
            response,
            thinking,
        })
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
        let reply_tool = build_reply_tool(&schemas)?;
        let (_, client) = self.pick_provider()?;
        let messages = {
            let mut history = self.messages.lock().await;
            if use_tool_choice_fallback {
                let new_message = build_reply_prompt_message(&schemas);
                history.push(new_message);
            }
            history.clone()
        };
        let messages = self.prepare_messages_for_request(messages, &request_options);
        let tool_choice = ToolChoice::tool(PROMPT_REPLY_TOOL_NAME, None);
        let system_prompt = system_prompt.trim();
        let system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };
        let thinking = self.thinking_payload_with_override(thinking.as_ref());
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
                    thinking,
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
                    tool_choice,
                    thinking,
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
            let event = event.whatever_context_display("read prompt reply stream error")?;
            let action = accumulator
                .handle_event(event, &mut on_update)
                .whatever_context("parse prompt reply stream error")?;
            if matches!(action, StreamAction::Stop) {
                break;
            }
        }

        let (blocks, stop_reason) = accumulator.finish();
        if !blocks.is_empty() {
            let msg = Message::assistant(Content::Multiple(blocks.clone()));
            self.messages.lock().await.push(msg);
        }
        let mut thinking = Vec::new();
        let mut reply_tool = None;
        for block in &blocks {
            match block {
                Block::Thinking { thinking: text, .. } => {
                    thinking.push(text.clone());
                }
                Block::ToolUse(tool_use) if tool_use.name == PROMPT_REPLY_TOOL_NAME => {
                    reply_tool = Some(tool_use.clone());
                }
                _ => (),
            }
        }
        let Some(tool_use) = reply_tool else {
            whatever!("reply tool use not found in response");
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
        Ok(PromptReply {
            tool_use,
            response,
            thinking,
        })
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

    pub fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
    }

    pub fn thinking_enabled(&self) -> bool {
        self.thinking_enabled
    }

    pub fn disable_stream_for_current_model(&self) -> bool {
        self.request_options_for_current_model().disable_stream
    }

    pub fn current_model(&self) -> String {
        let selected_model = self.selected_model();
        match Self::select_provider_index(selected_model.as_deref(), &self.config.providers) {
            Ok(idx) => {
                let provider = &self.config.providers[idx];
                Self::resolve_model(provider, selected_model)
            }
            Err(_) => selected_model.unwrap_or_else(|| "unknown".to_string()),
        }
    }

    pub fn resolved_default_model(&self) -> String {
        let default_model = self.default_model().map(|s| s.to_string());
        match Self::select_provider_index(default_model.as_deref(), &self.config.providers) {
            Ok(idx) => {
                let provider = &self.config.providers[idx];
                Self::resolve_model(provider, default_model)
            }
            Err(_) => default_model.unwrap_or_else(|| "unknown".to_string()),
        }
    }

    /// Check if the current provider has offload_combo_reply enabled.
    pub fn offload_combo_reply(&self) -> bool {
        let selected_model = self.selected_model();
        match Self::select_provider_index(selected_model.as_deref(), &self.config.providers) {
            Ok(idx) => {
                let provider = &self.config.providers[idx];
                let model = Self::resolve_model(provider, selected_model);
                let options = self.config.request_options_for_model(&model);
                options
                    .offload_combo_reply
                    .unwrap_or(provider.offload_combo_reply)
            }
            Err(_) => false,
        }
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
        self.executor
            .execute_with_output(id, name, input, cancel_token, on_output)
            .await
    }

    fn thinking_payload(&self) -> Option<Thinking> {
        if self.thinking_enabled {
            Some(Thinking::enabled(self.thinking_budget_tokens))
        } else {
            None
        }
    }

    fn thinking_payload_with_override(
        &self,
        thinking: Option<&ThinkingConfig>,
    ) -> Option<Thinking> {
        let Some(thinking) = thinking else {
            return self.thinking_payload();
        };
        if !thinking.enabled {
            return None;
        }
        let budget_tokens = thinking
            .budget_tokens
            .unwrap_or(self.thinking_budget_tokens);
        Some(Thinking::enabled(budget_tokens))
    }

    fn request_options_for_current_model(&self) -> RequestOptions {
        let selected_model = self.selected_model();
        match Self::select_provider_index(selected_model.as_deref(), &self.config.providers) {
            Ok(idx) => {
                let provider = &self.config.providers[idx];
                let model = Self::resolve_model(provider, selected_model);
                self.config.request_options_for_model(&model)
            }
            Err(_) => RequestOptions::default(),
        }
    }

    fn provider_tools_for_request(
        &self,
        request_options: &RequestOptions,
    ) -> Vec<crate::provider::Tool> {
        if request_options.disable_tools {
            Vec::new()
        } else {
            self.executor.provider_tools()
        }
    }

    fn strip_thinking_block(message: &Message) -> Option<Message> {
        let content = match &message.content {
            Content::Multiple(blocks) => {
                let filtered: Vec<Block> = blocks
                    .iter()
                    .filter(|block| !matches!(block, Block::Thinking { .. }))
                    .cloned()
                    .collect();
                Content::Multiple(filtered)
            }
            Content::Text(_) => message.content.clone(),
        };
        if matches!(content, Content::Multiple(ref blocks) if blocks.is_empty()) {
            None
        } else {
            Some(Message {
                role: message.role.clone(),
                content,
            })
        }
    }

    fn strip_thinking_blocks(messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .filter_map(Self::strip_thinking_block)
            .collect()
    }

    fn prepare_messages_for_request(
        &mut self,
        messages: Vec<Message>,
        request_options: &RequestOptions,
    ) -> Vec<Message> {
        if self.thinking_cleanup_pending && !request_options.include_reasoning_content {
            self.thinking_cleanup_pending = false;
            Self::strip_thinking_blocks(&messages)
        } else {
            messages
        }
    }

    fn mark_thinking_cleanup_pending(&mut self, reason: Option<&StopReason>) {
        if matches!(reason, Some(StopReason::EndTurn)) {
            self.thinking_cleanup_pending = true;
        }
    }

    /// Select the appropriate provider index for the given model.
    ///
    /// Selection algorithm:
    /// 1. If no model is specified, returns the first provider
    /// 2. If a model is specified, returns the first provider that:
    ///    - Matches provider name, OR
    ///    - Has the requested model in its models list
    /// 3. If no exact match, returns the first wildcard provider
    /// 4. If no matching provider, returns an error
    fn select_provider_index(
        agent_model: Option<&str>,
        providers: &[ProviderConfig],
    ) -> Result<usize> {
        if providers.is_empty() {
            whatever!("No providers configured")
        }

        // If no model specified, use first provider
        let Some(model) = agent_model else {
            return Ok(0);
        };

        // Exact match by provider name or declared models
        if let Some((idx, _)) = providers.iter().enumerate().find(|(_, provider)| {
            provider.name == model
                || provider
                    .models
                    .as_ref()
                    .is_some_and(|models| models.iter().any(|candidate| candidate == model))
        }) {
            return Ok(idx);
        }

        // Wildcard provider (no models specified or empty list)
        if let Some((idx, _)) = providers.iter().enumerate().find(|(_, provider)| {
            provider.models.is_none() || provider.models.as_ref().is_some_and(|m| m.is_empty())
        }) {
            return Ok(idx);
        }

        let available: Vec<_> = providers
            .iter()
            .map(|p| (p.name.clone(), p.models.clone()))
            .collect();
        whatever!(
            "No provider supports model '{}'. Available providers: {:?}",
            model,
            available
        )
    }

    fn pick_provider(&mut self) -> Result<(&str, Client)> {
        let selected_model = self.selected_model();

        let provider_idx =
            Self::select_provider_index(selected_model.as_deref(), &self.config.providers)?;
        let provider = &mut self.config.providers[provider_idx];

        // Use model override or agent's default_model if configured,
        // otherwise fallback to first provider.models,
        // otherwise fallback to provider.name
        let model = Self::resolve_model(provider, selected_model);

        let client = Client::new(provider, &model, crate::version::user_agent().to_string())?;
        Ok((&provider.name, client))
    }

    fn resolve_model(provider: &ProviderConfig, selected_model: Option<String>) -> String {
        selected_model.unwrap_or_else(|| {
            provider
                .models
                .as_ref()
                .and_then(|models| models.first().cloned())
                .unwrap_or_else(|| provider.name.to_owned())
        })
    }
}

fn build_reply_prompt_message(schemas: &[PromptSchema]) -> Message {
    Message::user(Content::Text(build_reply_tool_directive(schemas)))
}

fn build_reply_tool(schemas: &[PromptSchema]) -> Result<crate::provider::Tool> {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for schema in schemas {
        properties.insert(
            schema.name.clone(),
            json!({
                "type": "string",
                "description": schema.description.as_str(),
            }),
        );
        required.push(schema.name.clone());
    }
    ensure_whatever!(!properties.is_empty(), "schemas cannot be empty");
    let input_schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    });
    Ok(crate::provider::Tool {
        name: PROMPT_REPLY_TOOL_NAME.to_string(),
        description: "Return the response using the provided schema.".to_string(),
        input_schema,
    })
}

fn build_reply_tool_directive(schemas: &[PromptSchema]) -> String {
    let fields = schemas
        .iter()
        .map(|schema| schema.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "You must call the tool \"{PROMPT_REPLY_TOOL_NAME}\" exactly once. \
Do not output plain text. Provide all required fields in the tool input. \
Required fields: {fields}."
    )
}

fn should_use_tool_choice_fallback(request_options: &RequestOptions) -> bool {
    request_options.disable_tool_choice && request_options.tool_choice_fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_accumulator_updates_plain_and_thinking() {
        let mut accumulator = StreamAccumulator::new();
        let mut updates = Vec::new();
        let mut on_update = |update| updates.push(update);

        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: Block::text(""),
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta {
                        text: "Hello".to_string(),
                    },
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockStart {
                    index: 1,
                    content_block: Block::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockDelta {
                    index: 1,
                    delta: ContentBlockDelta::ThinkingDelta {
                        thinking: "Reasoning".to_string(),
                    },
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::MessageDelta {
                    delta: crate::provider::MessageDelta {
                        stop_reason: Some(StopReason::EndTurn),
                        stop_sequence: None,
                    },
                    usage: None,
                },
                &mut on_update,
            )
            .unwrap();
        let action = accumulator
            .handle_event(MessagesStreamEvent::MessageStop, &mut on_update)
            .unwrap();

        assert!(matches!(action, StreamAction::Stop));
        assert_eq!(updates.len(), 2);
        match &updates[0] {
            ChatStreamUpdate::Plain { index, text } => {
                assert_eq!(*index, 0);
                assert_eq!(text, "Hello");
            }
            other => panic!("unexpected update: {other:?}"),
        }
        match &updates[1] {
            ChatStreamUpdate::Thinking { index, text } => {
                assert_eq!(*index, 1);
                assert_eq!(text, "Reasoning");
            }
            other => panic!("unexpected update: {other:?}"),
        }

        let (blocks, stop_reason) = accumulator.finish();
        assert_eq!(stop_reason, Some(StopReason::EndTurn));
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Text { text } => assert_eq!(text, "Hello"),
            other => panic!("unexpected block: {other:?}"),
        }
        match &blocks[1] {
            Block::Thinking { thinking, .. } => assert_eq!(thinking, "Reasoning"),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn tool_choice_fallback_requires_disable() {
        let mut options = RequestOptions {
            tool_choice_fallback: true,
            ..Default::default()
        };
        assert!(!should_use_tool_choice_fallback(&options));
        options.disable_tool_choice = true;
        assert!(should_use_tool_choice_fallback(&options));
    }
}
