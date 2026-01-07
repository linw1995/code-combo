//! Agent module for managing AI assistants.
//!
//! This module provides:
//! - [`Agent`] - The main agent struct for chat interactions
//! - [`AgentConfig`] - Configuration types for customizing agents

use std::sync::Arc;

use anthropic::{Block as AnthropicBlock, Client, Thinking, Tool as AnthropicTool, ToolChoice};
use serde_json::{Map as JsonMap, json};
use snafu::prelude::*;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{Config, PromptSchema, ProviderConfig, Result, ThinkingConfig};
use executor::PermissionControl;
use prompt::{build_system_prompt_from_config, build_system_prompt_from_config_async};

mod bash_executor;
mod config;
mod executor;
mod prompt;

pub use anthropic::{Block, Content, Message, Role, StopReason, ToolUse};
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

    /// Full agent configuration loaded at initialization
    agent_config: AgentConfig,
}

pub struct ChatResponse {
    pub message: Message,
    pub stop_reason: Option<StopReason>,
}

pub struct PromptReply {
    pub tool_use: ToolUse,
    pub response: String,
    pub thinking: Vec<String>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let mut executor = Executor::default();

        let workspace_dir = config
            .workspace_config_path
            .as_deref()
            .and_then(|path| path.parent());

        // Configure bash safe commands
        bash_executor::configure_safe_commands(
            &config.bash_layers,
            &config.config_dir,
            workspace_dir,
        );

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
            agent_config,
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn set_system_prompt(&mut self, system_prompt: &str) {
        self.system_prompt = system_prompt.to_string()
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
        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload();

        let messages = {
            let mut messages = self.messages.lock().await;
            messages.push(message);
            messages.clone()
        };
        let messages = self.prepare_messages_for_request(messages);

        let response = client
            .messages()
            .system_prompt(&self.system_prompt)
            .conversations(messages)
            .tools(self.executor.anthropic_tools())
            .maybe_thinking(thinking)
            .call()
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .map_err(|err| {
                <crate::Error as snafu::FromString>::without_source(format!(
                    "send messages error: {err}"
                ))
            })?;

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
        let (_, client) = self.pick_provider()?;
        let thinking = self.thinking_payload();

        let messages = self.messages.lock().await.clone();
        let messages = self.prepare_messages_for_request(messages);
        let response = client
            .messages()
            .system_prompt(&self.system_prompt)
            .conversations(messages)
            .tools(self.executor.anthropic_tools())
            .maybe_thinking(thinking)
            .call()
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .map_err(|err| {
                <crate::Error as snafu::FromString>::without_source(format!(
                    "send messages error: {err}"
                ))
            })?;

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
        let reply_tool = build_reply_tool(&schemas)?;
        let (_, client) = self.pick_provider()?;
        let messages = {
            let mut history = self.messages.lock().await;
            let new_message = build_reply_prompt_message(&schemas);
            history.push(new_message);
            history.clone()
        };
        let messages = self.prepare_messages_for_request(messages);
        let tool_choice = ToolChoice::tool().name(PROMPT_REPLY_TOOL_NAME).call();
        let system_prompt = system_prompt.trim();
        let system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };
        let thinking = self.thinking_payload_with_override(thinking.as_ref());
        let response = client
            .messages_with_tool_choice(
                system_prompt,
                messages,
                vec![reply_tool],
                tool_choice,
                thinking,
            )
            .await
            .map_err(|err| {
                <crate::Error as snafu::FromString>::without_source(format!(
                    "failed to request prompt reply: {err}"
                ))
            })?;
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
                AnthropicBlock::Thinking { thinking: text, .. } => {
                    thinking.push(text);
                }
                AnthropicBlock::ToolUse(tool_use) if tool_use.name == PROMPT_REPLY_TOOL_NAME => {
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
            history.push(Message::user(Content::Multiple(vec![
                AnthropicBlock::tool_result(&tool_use.id, None, Content::Text("ok".to_string())),
            ])));
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

    fn prepare_messages_for_request(&mut self, messages: Vec<Message>) -> Vec<Message> {
        if self.thinking_cleanup_pending {
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

    /// Select the appropriate provider for the given model.
    ///
    /// Selection algorithm:
    /// 1. If no model is specified, returns the first provider
    /// 2. If a model is specified, returns the first provider that:
    ///    - Has no models field (wildcard), OR
    ///    - Has the requested model in its models list
    /// 3. If no matching provider, returns an error
    fn select_provider<'a>(
        agent_model: Option<&str>,
        providers: &'a mut [ProviderConfig],
    ) -> Result<&'a mut ProviderConfig> {
        if providers.is_empty() {
            whatever!("No providers configured")
        }

        // If no model specified, use first provider
        let Some(model) = agent_model else {
            return Ok(&mut providers[0]);
        };

        // Find provider index that supports this model
        let mut provider_index = None;
        for (idx, provider) in providers.iter().enumerate() {
            // Wildcard provider (no models specified or empty list)
            if provider.models.is_none() || provider.models.as_ref().is_some_and(|m| m.is_empty()) {
                provider_index = Some(idx);
                break;
            }

            // Exact match
            if let Some(models) = &provider.models
                && models.contains(&model.to_string())
            {
                provider_index = Some(idx);
                break;
            }
        }

        // Return provider or error
        match provider_index {
            Some(idx) => Ok(&mut providers[idx]),
            None => {
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
        }
    }

    fn pick_provider(&mut self) -> Result<(&str, Client)> {
        // Get default_model first to avoid borrow checker issues
        let default_model = self.default_model().map(|s| s.to_string());

        let provider = Self::select_provider(default_model.as_deref(), &mut self.config.providers)?;

        // Use agent's default_model if configured,
        // otherwise fallback to first provider.models,
        // otherwise fallback to provider.name
        let model = default_model.unwrap_or_else(|| {
            provider
                .models
                .to_owned()
                .and_then(|models| models.first().cloned())
                .unwrap_or(provider.name.to_owned())
        });

        let builder = Client::builder()
            .base_url(&provider.base_url)
            .token(provider.api_key.get()?)
            .model(&model)
            .user_agent(crate::version::user_agent().to_string());
        let client = builder.build().expect("Failed to initialize client");
        Ok((&provider.name, client))
    }
}

fn build_reply_prompt_message(schemas: &[PromptSchema]) -> Message {
    Message::user(Content::Text(build_reply_tool_directive(schemas)))
}

fn build_reply_tool(schemas: &[PromptSchema]) -> Result<AnthropicTool> {
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
    Ok(AnthropicTool {
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
