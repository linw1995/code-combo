use std::{
    collections::VecDeque,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use backon::{ExponentialBuilder, Retryable};
use bytes::Bytes;
use futures_core::Stream;
use reqwest::{StatusCode, Url, header::HeaderMap};
use snafu::{ResultExt, Whatever};
use tracing::{trace, warn};

use crate::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ErrorResponse,
    StreamOptions,
};

pub struct Client {
    model: String,
    base_url: Url,
    cli: reqwest::Client,
    has_version_prefix: bool,
}

#[derive(Debug, Clone)]
pub struct RetryAttempt {
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay: Duration,
    pub error: String,
}

pub type RetryNotifier = Arc<dyn Fn(RetryAttempt) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    Transport,
    Decode,
}

#[derive(Debug, Clone)]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
}

impl StreamError {
    fn transport(context: &'static str, err: impl std::fmt::Display) -> Self {
        Self {
            kind: StreamErrorKind::Transport,
            message: format!("{context}: {err}"),
        }
    }

    fn decode(context: &'static str, err: impl std::fmt::Display) -> Self {
        Self {
            kind: StreamErrorKind::Decode,
            message: format!("{context}: {err}"),
        }
    }
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StreamError {}

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

type Result<T> = std::result::Result<T, Whatever>;

impl Client {
    pub fn new(
        base_url: &str,
        token: &str,
        model: &str,
        user_agent: Option<String>,
    ) -> Result<Self> {
        let has_version_prefix = Self::check_version_prefix(base_url);
        let mut base_url = base_url.to_string();
        if !base_url.ends_with('/') {
            base_url.push('/');
        }
        let mut headers = HeaderMap::new();
        headers.append(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .expect("token is invalid as header value"),
        );
        let mut cli = reqwest::Client::builder().default_headers(headers);
        if let Some(user_agent) = user_agent {
            cli = cli.user_agent(user_agent);
        }
        Ok(Self {
            model: model.to_string(),
            base_url: base_url.parse().whatever_context("parse base url error")?,
            cli: cli.build().whatever_context("build client error")?,
            has_version_prefix,
        })
    }

    fn check_version_prefix(base_url: &str) -> bool {
        let trimmed = base_url.trim_end_matches('/');
        if let Some(last_segment) = trimmed.rsplit('/').next()
            && let Some(version) = last_segment.strip_prefix('v')
        {
            return !version.is_empty() && version.chars().all(|c| c.is_ascii_digit());
        }
        false
    }

    fn api_path<'a>(&self, endpoint: &'a str) -> &'a str {
        if self.has_version_prefix {
            endpoint
        } else {
            match endpoint {
                "chat/completions" => "v1/chat/completions",
                _ => endpoint,
            }
        }
    }

    pub async fn chat_completions(
        &self,
        mut request: ChatCompletionRequest,
        retry: RetryConfig,
    ) -> Result<ChatCompletionResponse> {
        request.model = self.model.clone();
        request.stream = None;
        request.stream_options = None;
        let url = self
            .base_url
            .join(self.api_path("chat/completions"))
            .whatever_context("build chat completions url")?;
        let cli = self.cli.clone();
        let request = request;
        let url_clone = url.clone();
        let notify = retry.notifier.clone();
        let max_attempts = retry.max_attempts;
        let mut attempts = 0usize;
        let backoff = build_backoff(retry);
        let result =
            (|| {
                let request = request.clone();
                let url = url_clone.clone();
                let cli = cli.clone();
                async move {
                    let resp =
                        cli.post(url).json(&request).send().await.map_err(|err| {
                            OpenAIRequestError::transport("chat completions", err)
                        })?;
                    let status = resp.status();
                    if !status.is_success() {
                        let body = resp
                            .text()
                            .await
                            .unwrap_or_else(|err| format!("failed to read error response: {err}"));
                        return Err(OpenAIRequestError::http_status(status, body));
                    }
                    resp.json::<ChatCompletionResponse>()
                        .await
                        .map_err(|err| OpenAIRequestError::decode("chat completions", err))
                }
            })
            .retry(backoff)
            .when(OpenAIRequestError::is_retryable)
            .notify(move |err, dur| {
                attempts = attempts.saturating_add(1);
                if let Some(notify) = notify.as_ref() {
                    notify(RetryAttempt {
                        attempt: attempts,
                        max_attempts,
                        delay: dur,
                        error: err.to_string(),
                    });
                }
                warn!(error = %err, delay = ?dur, "retrying openai chat completions request");
            })
            .await;
        result.map_err(OpenAIRequestError::into_whatever)
    }

    pub async fn chat_completions_stream(
        &self,
        mut request: ChatCompletionRequest,
        retry: RetryConfig,
    ) -> Result<ChatCompletionStream> {
        request.model = self.model.clone();
        request.stream = Some(true);
        request.stream_options = Some(StreamOptions {
            include_usage: Some(true),
        });
        let url = self
            .base_url
            .join(self.api_path("chat/completions"))
            .whatever_context("build chat completions stream url")?;
        let cli = self.cli.clone();
        let request = request;
        let url_clone = url.clone();
        let notify = retry.notifier.clone();
        let max_attempts = retry.max_attempts;
        let mut attempts = 0usize;
        let backoff = build_backoff(retry);
        let result = (|| {
            let request = request.clone();
            let url = url_clone.clone();
            let cli = cli.clone();
            async move {
                let resp =
                    cli.post(url).json(&request).send().await.map_err(|err| {
                        OpenAIRequestError::transport("chat completions stream", err)
                    })?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp
                        .text()
                        .await
                        .unwrap_or_else(|err| format!("failed to read error response: {err}"));
                    return Err(OpenAIRequestError::http_status(status, body));
                }
                Ok(ChatCompletionStream::new(resp))
            }
        })
        .retry(backoff)
        .when(OpenAIRequestError::is_retryable)
        .notify(move |err, dur| {
            attempts = attempts.saturating_add(1);
            if let Some(notify) = notify.as_ref() {
                notify(RetryAttempt {
                    attempt: attempts,
                    max_attempts,
                    delay: dur,
                    error: err.to_string(),
                });
            }
            warn!(error = %err, delay = ?dur, "retrying openai chat completions stream request");
        })
        .await;
        result.map_err(OpenAIRequestError::into_whatever)
    }
}

fn parse_error(status: StatusCode, body: &str) -> Whatever {
    if let Ok(err) = serde_json::from_str::<ErrorResponse>(body) {
        let mut message = format!("openai error ({status}): {}", err.error.message);
        if let Some(code) = err.error.code {
            message.push_str(&format!(" (code: {code})"));
        }
        if let Some(kind) = err.error.r#type {
            message.push_str(&format!(" (type: {kind})"));
        }
        <Whatever as snafu::FromString>::without_source(message)
    } else {
        <Whatever as snafu::FromString>::without_source(format!("openai error ({status}): {body}"))
    }
}

#[derive(Debug)]
enum OpenAIRequestError {
    Transport {
        context: &'static str,
        source: reqwest::Error,
    },
    HttpStatus {
        status: StatusCode,
        body: String,
    },
    Decode {
        context: &'static str,
        message: String,
    },
}

impl OpenAIRequestError {
    fn transport(context: &'static str, err: reqwest::Error) -> Self {
        Self::Transport {
            context,
            source: err,
        }
    }

    fn http_status(status: StatusCode, body: String) -> Self {
        Self::HttpStatus { status, body }
    }

    fn decode(context: &'static str, err: reqwest::Error) -> Self {
        Self::Decode {
            context,
            message: err.to_string(),
        }
    }

    fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { source, .. } => source.is_timeout() || source.is_connect(),
            Self::HttpStatus { status, .. } => is_retryable_status(*status),
            Self::Decode { .. } => true,
        }
    }

    fn into_whatever(self) -> Whatever {
        match self {
            Self::Transport { context, source } => <Whatever as snafu::FromString>::without_source(
                format!("send {context} request error: {source}"),
            ),
            Self::HttpStatus { status, body } => parse_error(status, &body),
            Self::Decode { context, message } => <Whatever as snafu::FromString>::without_source(
                format!("decode {context} response error: {message}"),
            ),
        }
    }
}

impl std::fmt::Display for OpenAIRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport { source, .. } => write!(f, "transport error: {source}"),
            Self::HttpStatus { status, .. } => write!(f, "http status {status}"),
            Self::Decode { message, .. } => write!(f, "decode error: {message}"),
        }
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::SERVICE_UNAVAILABLE
    )
}

fn build_backoff(retry: RetryConfig) -> ExponentialBuilder {
    let mut builder = ExponentialBuilder::default()
        .with_jitter()
        .with_max_times(retry.max_attempts);
    if retry.max_delay == Duration::from_millis(0) {
        builder = builder.without_max_delay();
    } else {
        builder = builder.with_max_delay(retry.max_delay);
    }
    builder
}

struct SseEvent {
    data: Vec<u8>,
}

struct SseEventStream {
    inner: Pin<Box<dyn Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send>>,
    buffer: Vec<u8>,
    pending: VecDeque<SseEvent>,
    current_data: Vec<u8>,
    finished: bool,
}

impl SseEventStream {
    fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            buffer: Vec::new(),
            pending: VecDeque::new(),
            current_data: Vec::new(),
            finished: false,
        }
    }

    fn pop_line(&mut self) -> Option<Vec<u8>> {
        let pos = self.buffer.iter().position(|b| *b == b'\n')?;
        let mut line: Vec<u8> = self.buffer.drain(..=pos).collect();
        if line.ends_with(b"\n") {
            line.pop();
        }
        if line.ends_with(b"\r") {
            line.pop();
        }
        Some(line)
    }

    fn process_line(&mut self, line: &[u8]) -> std::result::Result<(), StreamError> {
        if line.is_empty() {
            self.flush_event();
            return Ok(());
        }
        if let Some(data) = line.strip_prefix(b"data:") {
            let data = trim_leading_space(data);
            if !self.current_data.is_empty() {
                self.current_data.push(b'\n');
            }
            self.current_data.extend_from_slice(data);
        }
        Ok(())
    }

    fn flush_event(&mut self) {
        if self.current_data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.current_data);
        self.pending.push_back(SseEvent { data });
    }
}

impl Stream for SseEventStream {
    type Item = std::result::Result<SseEvent, StreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if this.finished {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Err(err))) => {
                    return Poll::Ready(Some(Err(StreamError::transport(
                        "read stream chunk error",
                        err,
                    ))));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                    while let Some(line) = this.pop_line() {
                        if let Err(err) = this.process_line(&line) {
                            return Poll::Ready(Some(Err(err)));
                        }
                    }
                    if let Some(event) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(event)));
                    }
                }
                Poll::Ready(None) => {
                    this.finished = true;
                    this.flush_event();
                    if let Some(event) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
            }
        }
    }
}

pub struct ChatCompletionStream {
    inner: SseEventStream,
    done: bool,
}

impl ChatCompletionStream {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: SseEventStream::new(resp.bytes_stream()),
            done: false,
        }
    }
}

impl Stream for ChatCompletionStream {
    type Item = std::result::Result<ChatCompletionChunk, StreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(Some(Ok(event))) => match parse_chat_completion_chunk(event) {
                Ok(Some(chunk)) => Poll::Ready(Some(Ok(chunk))),
                Ok(None) => {
                    this.done = true;
                    Poll::Ready(None)
                }
                Err(err) => Poll::Ready(Some(Err(err))),
            },
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
        }
    }
}

fn trim_leading_space(bytes: &[u8]) -> &[u8] {
    if bytes.first() == Some(&b' ') {
        &bytes[1..]
    } else {
        bytes
    }
}

fn parse_chat_completion_chunk(
    event: SseEvent,
) -> std::result::Result<Option<ChatCompletionChunk>, StreamError> {
    if event.data.is_empty() {
        return Ok(None);
    }
    let text = std::str::from_utf8(&event.data)
        .map_err(|err| StreamError::decode("stream event data is not utf-8", err))?;
    trace!(%text, "openai stream event");
    if text.trim() == "[DONE]" {
        return Ok(None);
    }
    let chunk: ChatCompletionChunk = serde_json::from_str(text)
        .map_err(|err| StreamError::decode("decode chat completion chunk", err))?;
    Ok(Some(chunk))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_status_codes() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn retryable_decode_errors() {
        let err = OpenAIRequestError::Decode {
            context: "chat completions",
            message: "bad json".to_string(),
        };
        assert!(err.is_retryable());
    }
}
