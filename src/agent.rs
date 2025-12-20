use std::sync::Arc;

use anthropic::Client;
use tokio::sync::Mutex;
use tracing::warn;

use super::Config;
use crate::Result;
use executor::PermissionControl;

mod executor;
pub use anthropic::{Block, Content, Message, Role, StopReason, ToolUse};
pub use executor::{Executor, Input, Output};

#[derive(Clone)]
pub struct Agent {
    config: Config,
    executor: Executor,
    /// Shared messages across cloned instances.
    messages: Arc<Mutex<Vec<Message>>>,
}

pub struct ChatResponse {
    pub message: Message,
    pub stop_reason: Option<StopReason>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            executor: Executor::default(),
            messages: Arc::new(Mutex::new(vec![])),
        }
    }

    pub async fn dump_messages(&self) -> Vec<Message> {
        self.messages.lock().await.clone()
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
        on_output: F,
    ) -> Result<()>
    where
        F: FnMut(Output) + Send,
    {
        self.executor
            .execute_with_output(id, name, input, on_output)
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
