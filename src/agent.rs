use std::sync::Arc;

use anthropic::Client;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::Config;
use crate::Result;
use executor::PermissionControl;

mod bash_executor;
mod executor;
pub use anthropic::{Block, Content, Message, Role, StopReason, ToolUse};
pub use executor::{ExecuteStatus, Executor, Input, Output};

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
}
