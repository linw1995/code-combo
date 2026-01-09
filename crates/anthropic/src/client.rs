mod messages;

use bon::bon;
pub use messages::*;
use reqwest::{Url, header::HeaderMap};
use snafu::{ResultExt, Whatever};

use crate::{Message, Tool};

pub struct Client {
    model: String,
    base_url: Url,
    cli: reqwest::Client,
}

type Result<T> = std::result::Result<T, Whatever>;

#[bon]
impl Client {
    #[builder]
    pub fn new(
        base_url: &str,
        token: &str,
        model: &str,
        user_agent: Option<String>,
    ) -> Result<Self> {
        let mut base_url = base_url.to_string();
        if !base_url.ends_with("/") {
            base_url.push('/');
        }
        let mut headers = HeaderMap::new();
        headers.append(
            "X-API-KEY",
            token.parse().expect("token is invalid as header value"),
        );
        headers.append("anthropic-version", "2023-06-01".parse().unwrap());
        let mut cli = reqwest::Client::builder().default_headers(headers);
        if let Some(user_agent) = user_agent {
            cli = cli.user_agent(user_agent);
        }
        Ok(Self {
            model: model.to_string(),
            base_url: base_url.parse().whatever_context("parse base url error")?,
            cli: cli.build().whatever_context("build client error")?,
        })
    }

    #[builder]
    pub async fn messages(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        thinking: Option<Thinking>,
    ) -> Result<MessagesResponse> {
        messages::messages(
            &self.cli,
            &self.base_url,
            MessagesRequest::builder()
                .maybe_system(system_prompt)
                .messages(conversations)
                .model(&self.model)
                .maybe_thinking(thinking)
                .tools(tools)
                .build(),
        )
        .await
    }

    pub async fn messages_with_tool_choice(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        tool_choice: ToolChoice,
        thinking: Option<Thinking>,
    ) -> Result<MessagesResponse> {
        messages::messages(
            &self.cli,
            &self.base_url,
            MessagesRequest {
                model: self.model.clone(),
                messages: conversations,
                max_tokens: 32000,
                system: system_prompt.unwrap_or_default().to_string(),
                temperature: None,
                tool_choice: Some(tool_choice),
                thinking,
                tools,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::{env, sync::OnceLock};

    use reqwest::{Client, Url, header::HeaderMap};

    pub static TEST_CLIENT: OnceLock<Client> = OnceLock::new();

    pub struct TestMetadata {
        pub base_url: Url,
        pub model: String,
    }

    /// Test Metadata
    pub fn test_md() -> TestMetadata {
        let mut base_url =
            env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8080/".into());

        if !base_url.ends_with("/") {
            base_url.push('/');
        }

        TestMetadata {
            base_url: base_url
                .parse()
                .expect("ANTHROPIC_BASE_URL environment variable is invalid as URL"),
            model: env::var("ANTHROPIC_MODEL")
                .expect("ANTHROPIC_MODEL environment variable is required")
                .to_string(),
        }
    }

    /// Test client
    pub fn test_client() -> &'static Client {
        TEST_CLIENT.get_or_init(|| {
            let token = env::var("ANTHROPIC_AUTH_TOKEN")
                .expect("ANTHROPIC_AUTH_TOKEN environment variable is required");
            let mut headers = HeaderMap::new();
            headers.append(
                "X-API-KEY",
                token
                    .parse()
                    .expect("ANTHROPIC_AUTH_TOKEN environment variable is invalid as header value"),
            );
            headers.append("anthropic-version", "2023-06-01".parse().unwrap());
            Client::builder()
                .default_headers(headers)
                .build()
                .expect("Failed to initialize HTTP client")
        })
    }

    #[test]
    fn test_user_agent_is_validated() {
        super::Client::builder()
            .base_url("http://localhost:8080/")
            .token("test-token")
            .model("test-model")
            .user_agent("test-agent/0.0.0".to_string())
            .build()
            .expect("Expected valid user agent to build");

        super::Client::builder()
            .base_url("http://localhost:8080/")
            .token("test-token")
            .model("test-model")
            .user_agent("test-agent/0.0.0\n".to_string())
            .build()
            .err()
            .expect("Expected invalid user agent to fail to build");
    }
}
