use std::sync::Arc;

use anthropic::{Block as AnthropicBlock, Client, Tool as AnthropicTool, ToolChoice};
use serde_json::{Map as JsonMap, json};
use snafu::prelude::*;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::Config;
use crate::{PromptSchema, ProviderKind, Result};
use executor::PermissionControl;

mod bash_executor;
mod executor;
pub use anthropic::{Block, Content, Message, Role, StopReason, ToolUse};
pub use executor::{ExecuteStatus, Executor, Input, Output};

const PROMPT_REPLY_TOOL_NAME: &str = "combo_reply";

#[derive(Clone)]
pub struct Agent {
    config: Config,
    executor: Executor,

    system_prompt: String,
    /// Shared messages across cloned instances.
    messages: Arc<Mutex<Vec<Message>>>,
}

pub struct ChatResponse {
    pub message: Message,
    pub stop_reason: Option<StopReason>,
}

pub struct PromptReply {
    pub tool_use: ToolUse,
    pub response: String,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let mut executor = Executor::default();
        executor.apply_tool_policies(config.allow_tools.as_deref(), config.deny_tools.as_deref());
        Self {
            config,
            system_prompt: String::new(),
            executor,
            messages: Arc::new(Mutex::new(vec![])),
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
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

        let mut messages = self.messages.lock().await;
        messages.push(message);

        let response = client
            .messages()
            .system_prompt(&self.system_prompt)
            .conversations(messages.clone())
            .tools(self.executor.anthropic_tools())
            .call()
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .unwrap();

        let message = if response.content.is_empty() {
            Message::assistant(Content::Multiple(Vec::default()))
        } else {
            let msg = Message::assistant(Content::Multiple(response.content));
            messages.push(msg.clone());
            msg
        };
        Ok(ChatResponse {
            message,
            stop_reason: response.stop_reason,
        })
    }

    pub async fn reply_prompt(
        &mut self,
        system_prompt: &str,
        prompt: String,
        schemas: Vec<PromptSchema>,
    ) -> Result<PromptReply> {
        ensure_whatever!(!schemas.is_empty(), "schemas cannot be empty");
        let reply_tool = build_reply_tool(&schemas)?;
        let client = self.build_reply_client()?;
        let new_messages = build_reply_prompt_messages(&prompt, &schemas);
        let messages = {
            let mut history = self.messages.lock().await;
            history.extend(new_messages.iter().cloned());
            history.clone()
        };
        let tool_choice = ToolChoice::tool().name(PROMPT_REPLY_TOOL_NAME).call();
        let system_prompt = system_prompt.trim();
        let system_prompt = if system_prompt.is_empty() {
            None
        } else {
            Some(system_prompt)
        };
        let response = client
            .messages_with_tool_choice(system_prompt, messages, vec![reply_tool], tool_choice)
            .await
            .map_err(|err| {
                <crate::Error as snafu::FromString>::without_source(format!(
                    "failed to request prompt reply: {err}"
                ))
            })?;
        if !response.content.is_empty() {
            let mut history = self.messages.lock().await;
            history.push(Message::assistant(Content::Multiple(
                response.content.clone(),
            )));
        }
        let Some(tool_use) = response.content.into_iter().find_map(|block| match block {
            AnthropicBlock::ToolUse(tool_use) if tool_use.name == PROMPT_REPLY_TOOL_NAME => {
                Some(tool_use)
            }
            _ => None,
        }) else {
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
        Ok(PromptReply { tool_use, response })
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

    fn build_reply_client(&mut self) -> Result<Client> {
        let Some(provider) = self.config.providers.first_mut() else {
            whatever!("no provider configured");
        };
        ensure_whatever!(
            matches!(provider.kind, ProviderKind::Anthropic),
            "only anthropic providers are supported for reply"
        );
        let token = provider.api_key.get()?;
        Client::builder()
            .base_url(&provider.base_url)
            .token(token)
            .model(&provider.name)
            .user_agent(crate::version::user_agent().to_string())
            .build()
            .map_err(|err| {
                <crate::Error as snafu::FromString>::without_source(format!(
                    "failed to build reply client: {err}"
                ))
            })
    }
}

fn build_reply_prompt_messages(prompt: &str, schemas: &[PromptSchema]) -> Vec<Message> {
    let mut messages = Vec::new();
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        messages.push(Message::user(Content::Text(prompt.to_string())));
    }
    messages.push(Message::user(Content::Text(build_reply_tool_directive(
        schemas,
    ))));
    messages
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
