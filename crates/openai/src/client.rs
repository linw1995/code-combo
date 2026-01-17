use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_core::Stream;
use reqwest::{StatusCode, Url, header::HeaderMap};
use snafu::{ResultExt, Whatever};
use tracing::trace;

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
    ) -> Result<ChatCompletionResponse> {
        request.model = self.model.clone();
        request.stream = None;
        request.stream_options = None;
        let url = self
            .base_url
            .join(self.api_path("chat/completions"))
            .whatever_context("build chat completions url")?;
        let resp = self
            .cli
            .post(url)
            .json(&request)
            .send()
            .await
            .whatever_context("send chat completions request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "failed to read error response".to_string());
            return Err(parse_error(status, &body));
        }
        resp.json::<ChatCompletionResponse>()
            .await
            .whatever_context("decode chat completions response")
    }

    pub async fn chat_completions_stream(
        &self,
        mut request: ChatCompletionRequest,
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
        let resp = self
            .cli
            .post(url)
            .json(&request)
            .send()
            .await
            .whatever_context("send chat completions stream request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "failed to read error response".to_string());
            return Err(parse_error(status, &body));
        }
        Ok(ChatCompletionStream::new(resp))
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

    fn process_line(&mut self, line: &[u8]) -> Result<()> {
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
    type Item = Result<SseEvent>;

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
                    let message = format!("read stream chunk error: {err}");
                    return Poll::Ready(Some(Err(
                        <Whatever as snafu::FromString>::without_source(message),
                    )));
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
    type Item = Result<ChatCompletionChunk>;

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

fn parse_chat_completion_chunk(event: SseEvent) -> Result<Option<ChatCompletionChunk>> {
    if event.data.is_empty() {
        return Ok(None);
    }
    let text =
        std::str::from_utf8(&event.data).whatever_context("stream event data is not utf-8")?;
    trace!(%text, "openai stream event");
    if text.trim() == "[DONE]" {
        return Ok(None);
    }
    let chunk: ChatCompletionChunk =
        serde_json::from_str(text).whatever_context("decode chat completion chunk")?;
    Ok(Some(chunk))
}
