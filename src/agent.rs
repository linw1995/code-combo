use anthropic::{Block, Client};
use tracing::warn;

use super::Config;

mod executor;
pub use anthropic::{Content, Message, Role};
pub use executor::Executor;

#[derive(Clone)]
pub struct Agent {
    #[allow(dead_code)]
    config: Config,
    messages: Vec<Message>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            messages: vec![],
        }
    }

    pub async fn chat(&mut self, message: Message) -> Vec<Message> {
        self.messages.push(message);

        let (_, client) = self.pick_provider();
        let response = client
            .messages(self.messages.clone())
            .await
            .inspect_err(|err| {
                warn!("send messsages error: {err:?}");
            })
            .unwrap();
        let messages = response
            .content
            .into_iter()
            .map(|content| match content {
                Block::Text { text } => Message::assistant(Content::Text(text)),
                _ => {
                    warn!(?content, "Unsupported response content block");
                    Message::assistant("Unsupported content block".into())
                }
            })
            .collect::<Vec<_>>();

        self.messages.extend(messages.clone());

        messages
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
