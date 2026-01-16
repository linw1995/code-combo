mod anthropic;
mod openai;
mod types;

use std::pin::Pin;

use futures_core::Stream;
use futures_util::StreamExt;
use snafu::Whatever;

use ::anthropic as anthropic_api;
use ::openai as openai_api;

use crate::{ProviderConfig, ProviderKind, RequestOptions, Result, ResultDisplayExt};

pub use types::*;

pub type MessagesStream =
    Pin<Box<dyn Stream<Item = std::result::Result<MessagesStreamEvent, Whatever>> + Send>>;

pub enum Client {
    Anthropic(anthropic_api::Client),
    OpenAI(openai_api::Client),
}

impl Client {
    pub fn new(provider: &mut ProviderConfig, model: &str, user_agent: String) -> Result<Self> {
        let token = provider.api_key.get()?;
        let client = match provider.kind {
            ProviderKind::Anthropic => {
                let builder = anthropic_api::Client::builder()
                    .base_url(&provider.base_url)
                    .token(token)
                    .model(model)
                    .user_agent(user_agent);
                Client::Anthropic(
                    builder
                        .build()
                        .whatever_context_display("failed to initialize anthropic client")?,
                )
            }
            ProviderKind::OpenAI => Client::OpenAI(
                openai_api::Client::new(&provider.base_url, token, model, Some(user_agent))
                    .whatever_context_display("failed to initialize openai client")?,
            ),
        };
        Ok(client)
    }

    pub async fn messages(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        thinking: Option<Thinking>,
        request_options: &RequestOptions,
    ) -> Result<MessagesResponse> {
        match self {
            Client::Anthropic(client) => {
                let temperature = request_options.temperature.map(f64::from);
                let max_tokens = request_options.max_tokens;
                let response = client
                    .messages()
                    .maybe_system_prompt(system_prompt)
                    .conversations(
                        conversations
                            .into_iter()
                            .map(anthropic_api::Message::from)
                            .collect(),
                    )
                    .tools(tools.into_iter().map(anthropic_api::Tool::from).collect())
                    .maybe_thinking(thinking.map(anthropic_api::Thinking::from))
                    .maybe_temperature(temperature)
                    .maybe_max_tokens(max_tokens)
                    .call()
                    .await
                    .whatever_context_display("failed to send messages")?;
                Ok(response.into())
            }
            Client::OpenAI(client) => openai::messages(
                client,
                system_prompt,
                conversations,
                tools,
                None,
                thinking,
                request_options,
            )
            .await
            .whatever_context_display("failed to send messages"),
        }
    }

    pub async fn messages_stream(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        thinking: Option<Thinking>,
        request_options: &RequestOptions,
    ) -> Result<MessagesStream> {
        match self {
            Client::Anthropic(client) => {
                let temperature = request_options.temperature.map(f64::from);
                let max_tokens = request_options.max_tokens;
                let stream = client
                    .messages_stream()
                    .maybe_system_prompt(system_prompt)
                    .conversations(
                        conversations
                            .into_iter()
                            .map(anthropic_api::Message::from)
                            .collect(),
                    )
                    .tools(tools.into_iter().map(anthropic_api::Tool::from).collect())
                    .maybe_thinking(thinking.map(anthropic_api::Thinking::from))
                    .maybe_temperature(temperature)
                    .maybe_max_tokens(max_tokens)
                    .call()
                    .await
                    .whatever_context_display("failed to send messages stream")?;
                let mapped = stream.map(|event| event.map(Into::into));
                Ok(Box::pin(mapped))
            }
            Client::OpenAI(client) => {
                let stream = openai::messages_stream(
                    client,
                    system_prompt,
                    conversations,
                    tools,
                    None,
                    thinking,
                    request_options,
                )
                .await
                .whatever_context_display("failed to send messages stream")?;
                Ok(Box::pin(stream))
            }
        }
    }

    pub async fn messages_with_tool_choice(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        tool_choice: ToolChoice,
        thinking: Option<Thinking>,
        request_options: &RequestOptions,
    ) -> Result<MessagesResponse> {
        match self {
            Client::Anthropic(client) => {
                let temperature = request_options.temperature.map(f64::from);
                let max_tokens = request_options.max_tokens;
                let response = client
                    .messages_with_tool_choice(
                        system_prompt,
                        conversations
                            .into_iter()
                            .map(anthropic_api::Message::from)
                            .collect(),
                        tools.into_iter().map(anthropic_api::Tool::from).collect(),
                        tool_choice.into(),
                        thinking.map(anthropic_api::Thinking::from),
                        temperature,
                        max_tokens,
                    )
                    .await
                    .whatever_context_display("failed to request tool choice")?;
                Ok(response.into())
            }
            Client::OpenAI(client) => openai::messages(
                client,
                system_prompt,
                conversations,
                tools,
                Some(tool_choice),
                thinking,
                request_options,
            )
            .await
            .whatever_context_display("failed to request tool choice"),
        }
    }

    pub async fn messages_stream_with_tool_choice(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        tool_choice: ToolChoice,
        thinking: Option<Thinking>,
        request_options: &RequestOptions,
    ) -> Result<MessagesStream> {
        match self {
            Client::Anthropic(client) => {
                let temperature = request_options.temperature.map(f64::from);
                let max_tokens = request_options.max_tokens;
                let stream = client
                    .messages_stream_with_tool_choice(
                        system_prompt,
                        conversations
                            .into_iter()
                            .map(anthropic_api::Message::from)
                            .collect(),
                        tools.into_iter().map(anthropic_api::Tool::from).collect(),
                        tool_choice.into(),
                        thinking.map(anthropic_api::Thinking::from),
                        temperature,
                        max_tokens,
                    )
                    .await
                    .whatever_context_display("failed to request tool choice stream")?;
                let mapped = stream.map(|event| event.map(Into::into));
                Ok(Box::pin(mapped))
            }
            Client::OpenAI(client) => {
                let stream = openai::messages_stream(
                    client,
                    system_prompt,
                    conversations,
                    tools,
                    Some(tool_choice),
                    thinking,
                    request_options,
                )
                .await
                .whatever_context_display("failed to request tool choice stream")?;
                Ok(Box::pin(stream))
            }
        }
    }
}
