use std::sync::Arc;

use anthropic::{Block as AnthropicBlock, Client, Thinking, Tool as AnthropicTool, ToolChoice};
use indoc::indoc;
use serde_json::{Map as JsonMap, json};
use snafu::prelude::*;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::Config;
use crate::{PromptSchema, Result, ThinkingConfig};
use executor::PermissionControl;

mod bash_executor;
mod executor;
pub use anthropic::{Block, Content, Message, Role, StopReason, ToolUse};
pub use bash_executor::{bash_unsafe_ranges, bash_unsafe_reason};
pub use executor::{ExecuteStatus, Executor, Input, Output};

const PROMPT_REPLY_TOOL_NAME: &str = "combo_reply";
const DEFAULT_THINKING_BUDGET_TOKENS: usize = 1024;
const BUILTIN_SYSTEM_PROMPT: &str = indoc! {"
    You are Coco, a coding assistant running inside the code-combo CLI.
    Introduce yourself briefly when a new conversation starts.
    When you need to offload work to MCP tools, use the MCP offloading CLI.
    Discover servers with `coco mcp -h`, list tools with `coco mcp <server> -h`,
    and list tool arguments with `coco mcp <server> <tool> --help`.
"};

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
        executor.apply_tool_policies(config.allow_tools.as_deref(), config.deny_tools.as_deref());
        let workspace_dir = config
            .workspace_config_path
            .as_deref()
            .and_then(|path| path.parent());
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
        Self {
            config,
            system_prompt: Self::build_system_prompt(None),
            executor,
            messages: Arc::new(Mutex::new(vec![])),
            thinking_enabled: false,
            thinking_budget_tokens,
            thinking_cleanup_pending: false,
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn build_system_prompt(custom: Option<&str>) -> String {
        match custom {
            Some(value) if !value.trim().is_empty() => {
                format!("{BUILTIN_SYSTEM_PROMPT}\n\n{value}")
            }
            _ => BUILTIN_SYSTEM_PROMPT.to_string(),
        }
    }

    pub fn set_system_prompt(&mut self, system_prompt: &str) {
        self.system_prompt = system_prompt.to_string()
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

    fn pick_provider(&mut self) -> Result<(&str, Client)> {
        // TODO: pick a provider based on some strategy
        let first = self.config.providers.first_mut();
        let provider = first.unwrap();
        let builder = Client::builder()
            .base_url(&provider.base_url)
            .token(provider.api_key.get()?)
            .model(&provider.name)
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
