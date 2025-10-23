use anthropic_api::{
    Credentials,
    messages::{MessagesBuilder, ResponseContentBlock},
};
use tracing::warn;

use super::Config;

mod executor;
pub use anthropic_api::messages::{Message, MessageContent, MessageRole};
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

        let (model, credentials) = self.pick_provider();
        let response = MessagesBuilder::builder(model, self.messages.clone(), 1024)
            .credentials(credentials)
            .create()
            .await
            .unwrap();
        let messages = response
            .content
            .into_iter()
            .map(|content| match content {
                ResponseContentBlock::Text { text } => Message {
                    role: MessageRole::Assistant,
                    content: MessageContent::Text(text),
                },
                _ => {
                    warn!(?content, "Unsupported response content block");
                    Message {
                        role: MessageRole::Assistant,
                        content: MessageContent::Text(
                            "Unsupported response content block".to_string(),
                        ),
                    }
                }
            })
            .collect::<Vec<_>>();

        self.messages.extend(messages.clone());

        messages
    }

    fn pick_provider(&self) -> (&str, Credentials) {
        // TODO: pick a provider based on some strategy
        let first = self.config.providers.first();
        let provider = first.unwrap();
        (
            &provider.name,
            Credentials::new(provider.api_key.clone(), provider.base_url.clone()),
        )
    }
}
