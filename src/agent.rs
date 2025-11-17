use std::sync::Arc;

use anthropic::Client;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::warn;

use super::Config;
use executor::PermissionControl;

mod executor;
pub use anthropic::{Block, Content, Message, Role, ToolUse};
pub use executor::{ExecuteOutput, Executor};

#[derive(Clone)]
pub struct Agent {
    config: Config,
    executor: Executor,
    /// Shared messages across cloned instances.
    messages: Arc<Mutex<Vec<Message>>>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            executor: Executor::default(),
            messages: Arc::new(Mutex::new(vec![])),
        }
    }

    pub async fn chat(&mut self, message: Message) -> Message {
        let mut messages = self.messages.lock().await;
        messages.push(message);

        let (_, client) = self.pick_provider();
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

        let message = Message::assistant(Content::Multiple(response.content));
        messages.push(message.clone());

        message
    }

    pub fn grant_once(&mut self, id: &str, name: &str) {
        self.executor
            .update_pcl(name, PermissionControl::Once(id.to_string()))
    }

    pub async fn execute(&mut self, id: &str, name: &str, input: Value) -> ExecuteOutput {
        self.executor
            .execute(id, name, input)
            .await
            .expect("Failed to execute")
    }

    fn pick_provider(&self) -> (&str, Client) {
        // TODO: pick a provider based on some strategy
        let first = self.config.providers.first();
        let provider = first.unwrap();
        (
            &provider.name,
            Client::builder()
                .base_url(&provider.base_url)
                .token(&provider.api_key)
                .model(&provider.name)
                .build()
                .expect("Failed to initialize client"),
        )
    }
}
