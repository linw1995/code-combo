use anthropic::Client;
use serde_json::Value;
use tracing::warn;

use super::Config;
use executor::PermissionControl;

mod executor;
pub use anthropic::{Block, Content, Message, Role, ToolUse};
pub use executor::{ExecuteOutput, Executor};

#[derive(Clone)]
pub struct Agent {
    #[allow(dead_code)]
    config: Config,
    executor: Executor,
    messages: Vec<Message>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            executor: Executor::default(),
            messages: vec![],
        }
    }

    pub async fn chat(&mut self, message: Message) -> Message {
        self.messages.push(message);

        let (_, client) = self.pick_provider();
        let response = client
            .messages()
            .conversations(self.messages.clone())
            .tools(self.executor.anthropic_tools())
            .call()
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .unwrap();

        let message = Message::assistant(Content::Multiple(response.content));
        self.messages.push(message.clone());

        message
    }

    pub fn grant_once(&mut self, id: &str, name: &str) {
        self.executor
            .update_pcl(name, PermissionControl::Once(id.to_string()))
    }

    pub async fn execute(&mut self, id: &str, name: &str, input: Value) -> ExecuteOutput<Value> {
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
