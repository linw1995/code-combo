mod anthropic;
mod openai;
mod types;

use std::{pin::Pin, time::Duration};

use crate::StreamError;
use futures_core::Stream;
use futures_util::StreamExt;

use ::anthropic as anthropic_api;
use ::openai as openai_api;

use crate::{
    ProviderConfig, ProviderKind, RequestOptions, Result, ResultDisplayExt,
    RetryAttempt as CoreRetryAttempt, RetryUpdate,
};

pub use types::*;

pub type MessagesStream =
    Pin<Box<dyn Stream<Item = std::result::Result<MessagesStreamEvent, StreamError>> + Send>>;

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
        let result: Result<MessagesResponse> = match self {
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
                    .retry_config(anthropic_retry_config(request_options))
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
        };
        notify_retry_finished(request_options, result.is_ok());
        result
    }

    pub async fn messages_stream(
        &self,
        system_prompt: Option<&str>,
        conversations: Vec<Message>,
        tools: Vec<Tool>,
        thinking: Option<Thinking>,
        request_options: &RequestOptions,
    ) -> Result<MessagesStream> {
        let result: Result<MessagesStream> = match self {
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
                    .retry_config(anthropic_retry_config(request_options))
                    .call()
                    .await
                    .whatever_context_display("failed to send messages stream")?;
                let mapped =
                    stream.map(|event| event.map(Into::into).map_err(map_anthropic_stream_error));
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
        };
        notify_retry_finished(request_options, result.is_ok());
        result
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
        let result: Result<MessagesResponse> = match self {
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
                        anthropic_retry_config(request_options),
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
        };
        notify_retry_finished(request_options, result.is_ok());
        result
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
        let result: Result<MessagesStream> = match self {
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
                        anthropic_retry_config(request_options),
                    )
                    .await
                    .whatever_context_display("failed to request tool choice stream")?;
                let mapped =
                    stream.map(|event| event.map(Into::into).map_err(map_anthropic_stream_error));
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
        };
        notify_retry_finished(request_options, result.is_ok());
        result
    }
}

fn notify_retry_finished(request_options: &RequestOptions, success: bool) {
    if let Some(notifier) = &request_options.retry_notifier {
        notifier.notify(RetryUpdate::Finished { success });
    }
}

fn anthropic_retry_config(request_options: &RequestOptions) -> anthropic_api::RetryConfig {
    let notifier = request_options.retry_notifier.clone().map(|notifier| {
        let inner: anthropic_api::RetryNotifier =
            std::sync::Arc::new(move |attempt: anthropic_api::RetryAttempt| {
                notifier.notify(RetryUpdate::Attempt(CoreRetryAttempt {
                    attempt: attempt.attempt,
                    max_attempts: attempt.max_attempts,
                    delay: attempt.delay,
                    error: attempt.error,
                }));
            });
        inner
    });
    anthropic_api::RetryConfig {
        max_attempts: request_options.retry_max_attempts,
        max_delay: Duration::from_millis(request_options.retry_max_delay_ms),
        notifier,
    }
}

fn map_anthropic_stream_error(err: anthropic_api::StreamError) -> StreamError {
    match err.kind {
        anthropic_api::StreamErrorKind::Transport => StreamError::transport(err.message),
        anthropic_api::StreamErrorKind::Decode => StreamError::decode(err.message),
    }
}
