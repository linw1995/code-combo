pub mod messages;

use std::{sync::Arc, time::Duration};

use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue},
};
use snafu::{Whatever, prelude::*};

use crate::{Message, Tool};

pub use messages::*;

const DEFAULT_MAX_TOKENS: usize = 32000;
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct Client {
    model: String,
    base_url: Url,
    cli: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct RetryAttempt {
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay: Duration,
    pub error: String,
}

pub type RetryNotifier = Arc<dyn Fn(RetryAttempt) + Send + Sync>;

#[derive(Clone)]
pub struct RetryConfig {
    pub max_attempts: usize,
    pub max_delay: Duration,
    pub notifier: Option<RetryNotifier>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            max_delay: Duration::from_secs(60),
            notifier: None,
        }
    }
}

#[derive(Default)]
pub struct ClientBuilder {
    base_url: Option<String>,
    token: Option<String>,
    model: Option<String>,
    user_agent: Option<String>,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    pub fn messages(&self) -> MessagesBuilder<'_> {
        MessagesBuilder::new(self)
    }

    pub fn messages_stream(&self) -> MessagesStreamBuilder<'_> {
        MessagesStreamBuilder::new(self)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn messages_with_tool_choice(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        tool_choice: ToolChoice,
        thinking: Option<Thinking>,
        temperature: Option<f64>,
        max_tokens: Option<usize>,
        retry_config: RetryConfig,
    ) -> Result<messages::MessagesResponse, Whatever> {
        let req = build_request(
            &self.model,
            system_prompt,
            conversations,
            tools,
            Some(tool_choice),
            thinking,
            temperature,
            max_tokens,
        );
        messages::messages(&self.cli, &self.base_url, req, retry_config).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn messages_stream_with_tool_choice(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        tool_choice: ToolChoice,
        thinking: Option<Thinking>,
        temperature: Option<f64>,
        max_tokens: Option<usize>,
        retry_config: RetryConfig,
    ) -> Result<messages::MessagesStream, Whatever> {
        let req = build_request(
            &self.model,
            system_prompt,
            conversations,
            tools,
            Some(tool_choice),
            thinking,
            temperature,
            max_tokens,
        );
        messages::messages_stream(&self.cli, &self.base_url, req, retry_config).await
    }
}

impl ClientBuilder {
    pub fn base_url(mut self, base_url: &str) -> Self {
        self.base_url = Some(base_url.to_string());
        self
    }

    pub fn token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = Some(model.to_string());
        self
    }

    pub fn user_agent(mut self, user_agent: String) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    pub fn build(self) -> Result<Client, Whatever> {
        let base_url = self.base_url.ok_or_else(|| missing_field("base url"))?;
        let token = self.token.ok_or_else(|| missing_field("token"))?;
        let model = self.model.ok_or_else(|| missing_field("model"))?;
        let mut base_url = base_url;
        if !base_url.ends_with('/') {
            base_url.push('/');
        }
        let base_url = base_url
            .parse::<Url>()
            .whatever_context("parse anthropic base url")?;
        let headers = build_headers(&token)?;
        let mut cli = reqwest::Client::builder().default_headers(headers);
        if let Some(user_agent) = self.user_agent {
            cli = cli.user_agent(user_agent);
        }
        let cli = cli.build().whatever_context("build anthropic client")?;
        Ok(Client {
            model,
            base_url,
            cli,
        })
    }
}

pub struct MessagesBuilder<'a> {
    client: &'a Client,
    system_prompt: Option<String>,
    conversations: Vec<Message>,
    tools: Vec<Tool>,
    thinking: Option<Thinking>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
    retry_config: RetryConfig,
}

impl<'a> MessagesBuilder<'a> {
    fn new(client: &'a Client) -> Self {
        Self {
            client,
            system_prompt: None,
            conversations: Vec::new(),
            tools: Vec::new(),
            thinking: None,
            temperature: None,
            max_tokens: None,
            retry_config: RetryConfig::default(),
        }
    }

    pub fn maybe_system_prompt(mut self, system_prompt: Option<&str>) -> Self {
        self.system_prompt = system_prompt.map(str::to_string);
        self
    }

    pub fn conversations(mut self, conversations: Vec<Message>) -> Self {
        self.conversations = conversations;
        self
    }

    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    pub fn maybe_thinking(mut self, thinking: Option<Thinking>) -> Self {
        self.thinking = thinking;
        self
    }

    pub fn maybe_temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn maybe_max_tokens(mut self, max_tokens: Option<usize>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    pub async fn call(self) -> Result<messages::MessagesResponse, Whatever> {
        let req = build_request(
            &self.client.model,
            self.system_prompt.as_deref(),
            self.conversations,
            self.tools,
            None,
            self.thinking,
            self.temperature,
            self.max_tokens,
        );
        messages::messages(
            &self.client.cli,
            &self.client.base_url,
            req,
            self.retry_config,
        )
        .await
    }
}

pub struct MessagesStreamBuilder<'a> {
    client: &'a Client,
    system_prompt: Option<String>,
    conversations: Vec<Message>,
    tools: Vec<Tool>,
    thinking: Option<Thinking>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
    retry_config: RetryConfig,
}

impl<'a> MessagesStreamBuilder<'a> {
    fn new(client: &'a Client) -> Self {
        Self {
            client,
            system_prompt: None,
            conversations: Vec::new(),
            tools: Vec::new(),
            thinking: None,
            temperature: None,
            max_tokens: None,
            retry_config: RetryConfig::default(),
        }
    }

    pub fn maybe_system_prompt(mut self, system_prompt: Option<&str>) -> Self {
        self.system_prompt = system_prompt.map(str::to_string);
        self
    }

    pub fn conversations(mut self, conversations: Vec<Message>) -> Self {
        self.conversations = conversations;
        self
    }

    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    pub fn maybe_thinking(mut self, thinking: Option<Thinking>) -> Self {
        self.thinking = thinking;
        self
    }

    pub fn maybe_temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn maybe_max_tokens(mut self, max_tokens: Option<usize>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    pub async fn call(self) -> Result<messages::MessagesStream, Whatever> {
        let req = build_request(
            &self.client.model,
            self.system_prompt.as_deref(),
            self.conversations,
            self.tools,
            None,
            self.thinking,
            self.temperature,
            self.max_tokens,
        );
        messages::messages_stream(
            &self.client.cli,
            &self.client.base_url,
            req,
            self.retry_config,
        )
        .await
    }
}

fn build_headers(token: &str) -> Result<HeaderMap, Whatever> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-api-key",
        token
            .parse()
            .whatever_context("anthropic token is invalid header")?,
    );
    headers.insert(
        "anthropic-version",
        HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    Ok(headers)
}

fn missing_field(field: &str) -> Whatever {
    <Whatever as snafu::FromString>::without_source(format!("missing anthropic {field}"))
}

#[allow(clippy::too_many_arguments)]
fn build_request(
    model: &str,
    system_prompt: Option<&str>,
    conversations: Vec<Message>,
    tools: Vec<Tool>,
    tool_choice: Option<messages::ToolChoice>,
    thinking: Option<messages::Thinking>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
) -> messages::MessagesRequest {
    messages::MessagesRequest {
        model: model.to_string(),
        messages: conversations,
        max_tokens: max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        system: system_prompt.unwrap_or_default().to_string(),
        temperature,
        tool_choice,
        thinking,
        stream: None,
        tools,
    }
}

#[cfg(test)]
pub mod tests {
    use reqwest::{
        Client, Url,
        header::{HeaderMap, HeaderValue},
    };
    use snafu::{Whatever, prelude::*};

    const ANTHROPIC_VERSION: &str = "2023-06-01";

    pub struct TestMetadata {
        pub base_url: Url,
        pub model: String,
        pub token: String,
    }

    pub fn test_md() -> Option<TestMetadata> {
        let base_url = std::env::var("ANTHROPIC_BASE_URL").ok()?;
        let model = std::env::var("ANTHROPIC_MODEL").ok()?;
        let token = std::env::var("ANTHROPIC_API_KEY").ok()?;
        let base_url = base_url.parse::<Url>().ok()?;
        Some(TestMetadata {
            base_url,
            model,
            token,
        })
    }

    pub fn test_client(token: &str) -> Result<Client, Whatever> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            token
                .parse()
                .whatever_context("anthropic token is invalid header")?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .whatever_context("build test client")
    }
}
