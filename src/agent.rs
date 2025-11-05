use anthropic::Client;
use serde_json::Value;
use tracing::{debug, warn};

use super::Config;

mod executor;
pub use anthropic::{Block, Content, Message, Role};
pub use executor::Executor;

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

    pub async fn execute(&mut self, name: &str, input: Value) {
        let rv = self.executor.execute("", name, input).await;
        debug!("[tmp] executed result: {rv:?}")
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
