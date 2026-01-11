use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

use bon::bon;
use bytes::Bytes;
use futures_core::Stream;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use snafu::{Whatever, prelude::*};
use tracing::trace;

use crate::{Block, Message, Role, Tool};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Thinking {
    Enabled { budget_tokens: usize },
}

impl Thinking {
    pub fn enabled(budget_tokens: usize) -> Self {
        Self::Enabled { budget_tokens }
    }
}

#[bon]
impl ToolChoice {
    #[builder]
    pub fn auto(disable_parallel_tool_use: Option<bool>) -> Self {
        Self::Auto {
            disable_parallel_tool_use,
        }
    }

    #[builder]
    pub fn any(disable_parallel_tool_use: Option<bool>) -> Self {
        Self::Any {
            disable_parallel_tool_use,
        }
    }

    #[builder]
    pub fn tool(name: &str, disable_parallel_tool_use: Option<bool>) -> Self {
        Self::Tool {
            name: name.to_string(),
            disable_parallel_tool_use,
        }
    }

    pub fn none() -> Self {
        Self::None
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessagesRequest {
    /// The model that will complete your prompt.
    pub model: String,
    /// Input messages.
    pub messages: Vec<Message>,
    /// The maximum number of tokens to generate before stopping.
    pub max_tokens: usize,
    /// System prompt
    pub system: String,
    /// Amount of randomness injected into the response.
    ///
    /// Defaults to 1.0. Ranges from 0.0 to 1.0. Use temperature closer to 0.0 for analytical / multiple choice,
    /// and closer to 1.0 for creative and generative tasks.
    /// Note that even with temperature of 0.0, the results will not be fully deterministic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// How the model should use the provided tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Enable thinking mode for the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    /// Enable streaming responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Definitions of tools that the model may use.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

#[bon]
impl MessagesRequest {
    #[builder]
    pub fn new(
        model: &str,
        system: Option<&str>,
        messages: Vec<Message>,
        #[builder(default)] tools: Vec<Tool>,
        thinking: Option<Thinking>,
    ) -> Self {
        Self {
            model: model.to_string(),
            messages,
            system: system.unwrap_or_default().to_string(),
            max_tokens: 32000,
            temperature: None,
            tool_choice: None,
            thinking,
            stream: None,
            tools,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// end_turn: the model reached a natural stopping point
    EndTurn,
    /// max_tokens: we exceeded the requested max_tokens or the model's maximum
    MaxTokens,
    /// stop_sequence: one of your provided custom stop_sequences was generated
    StopSequence,
    /// tool_use: the model invoked one or more tools
    ToolUse,
    /// pause_turn: we paused a long-running turn.
    /// You may provide the response back as-is in a subsequent request to let the model continue.
    PauseTurn,
    /// refusal: when streaming classifiers intervene to handle potential policy violations
    Refusal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CacheCreation {
    /// The number of input tokens used to create the 1 hour cache entry.
    pub ephemeral_1h_input_tokens: usize,
    /// The number of input tokens used to create the 5 minute cache entry.
    pub ephemeral_5m_input_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerToolUse {
    /// The number of web fetch tool requests.
    pub web_fetch_requests: usize,
    /// The number of web search tool requests.
    pub web_search_requests: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    Standard,
    Priority,
    Batch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Usage {
    /// Breakdown of cached tokens by TTL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The number of input tokens used to create the cache entry.
    pub cache_creation_input_tokens: Option<usize>,
    /// The number of input tokens read from the cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<usize>,
    /// The number of input tokens which were used.
    pub input_tokens: usize,
    /// The number of output tokens which were used.
    pub output_tokens: usize,
    /// The number of server tool requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUse>,
    /// If the request used the priority, standard, or batch tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessagesResponse {
    pub id: String,
    /// The model that handled the request.
    pub model: String,
    /// For Messages, this is always "message".
    pub r#type: String,
    /// This will always be "assistant".
    pub role: Role,
    /// Note: The Block type from Server has different sets of types. However, reusing the client
    /// definition is sufficient for now.
    pub content: Vec<Block>,
    /// The reason that we stopped.
    pub stop_reason: Option<StopReason>,
    /// Which custom stop sequence was generated, if any.
    pub stop_sequence: Option<String>,
    /// Billing and rate-limit usage.
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamUsage {
    /// Breakdown of cached tokens by TTL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The number of input tokens used to create the cache entry.
    pub cache_creation_input_tokens: Option<usize>,
    /// The number of input tokens read from the cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<usize>,
    /// The number of input tokens which were used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<usize>,
    /// The number of output tokens which were used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<usize>,
    /// The number of server tool requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUse>,
    /// If the request used the priority, standard, or batch tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ContentBlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamErrorDetail {
    pub message: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MessagesStreamEvent {
    MessageStart {
        message: MessagesResponse,
    },
    ContentBlockStart {
        index: usize,
        content_block: Block,
    },
    ContentBlockDelta {
        index: usize,
        delta: ContentBlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: Option<StreamUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: StreamErrorDetail,
    },
    Unknown {
        event: String,
        data: Value,
    },
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    message: String,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

struct SseEvent {
    event: String,
    data: Vec<u8>,
}

struct SseEventStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<u8>,
    pending: VecDeque<SseEvent>,
    finished: bool,
}

impl SseEventStream {
    fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            buffer: Vec::new(),
            event: None,
            data: Vec::new(),
            pending: VecDeque::new(),
            finished: false,
        }
    }

    fn pop_line(&mut self) -> Option<Vec<u8>> {
        let pos = self.buffer.iter().position(|&byte| byte == b'\n')?;
        let mut line = self.buffer.drain(..=pos).collect::<Vec<u8>>();
        if line.ends_with(b"\n") {
            line.pop();
        }
        if line.ends_with(b"\r") {
            line.pop();
        }
        Some(line)
    }

    fn process_line(&mut self, line: &[u8]) -> Result<(), Whatever> {
        if line.is_empty() {
            self.flush_event();
            return Ok(());
        }
        if line.first() == Some(&b':') {
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix(b"event:") {
            let rest = trim_leading_space(rest);
            let name =
                std::str::from_utf8(rest).whatever_context("stream event name is not utf-8")?;
            self.event = Some(name.to_string());
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix(b"data:") {
            let rest = trim_leading_space(rest);
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(rest);
        }
        Ok(())
    }

    fn flush_event(&mut self) {
        if self.event.is_none() && self.data.is_empty() {
            return;
        }
        let event = self.event.take().unwrap_or_else(|| "message".to_string());
        let data = std::mem::take(&mut self.data);
        self.pending.push_back(SseEvent { event, data });
    }
}

impl Stream for SseEventStream {
    type Item = Result<SseEvent, Whatever>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(event) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }
        if this.finished {
            return Poll::Ready(None);
        }
        loop {
            match this.inner.as_mut().poll_next(cx) {
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

pub struct MessagesStream {
    inner: SseEventStream,
}

impl MessagesStream {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: SseEventStream::new(resp.bytes_stream()),
        }
    }

    #[cfg(test)]
    fn from_bytes_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        Self {
            inner: SseEventStream::new(stream),
        }
    }
}

impl Stream for MessagesStream {
    type Item = Result<MessagesStreamEvent, Whatever>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(parse_messages_stream_event(event))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
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

fn parse_messages_stream_event(event: SseEvent) -> Result<MessagesStreamEvent, Whatever> {
    let data = if event.data.is_empty() {
        Value::Object(Map::new())
    } else {
        let text =
            std::str::from_utf8(&event.data).whatever_context("stream event data is not utf-8")?;
        serde_json::from_str::<Value>(text).whatever_context("decode stream event data")?
    };
    let event_type = data
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or(event.event.as_str())
        .to_string();
    match event_type.as_str() {
        "message_start" => {
            let event: MessageStartEvent =
                serde_json::from_value(data).whatever_context("decode message_start event")?;
            Ok(MessagesStreamEvent::MessageStart {
                message: event.message,
            })
        }
        "content_block_start" => {
            let event: ContentBlockStartEvent = serde_json::from_value(data)
                .whatever_context("decode content_block_start event")?;
            Ok(MessagesStreamEvent::ContentBlockStart {
                index: event.index,
                content_block: event.content_block,
            })
        }
        "content_block_delta" => {
            let event: ContentBlockDeltaEvent = serde_json::from_value(data)
                .whatever_context("decode content_block_delta event")?;
            Ok(MessagesStreamEvent::ContentBlockDelta {
                index: event.index,
                delta: event.delta,
            })
        }
        "content_block_stop" => {
            let event: ContentBlockStopEvent =
                serde_json::from_value(data).whatever_context("decode content_block_stop event")?;
            Ok(MessagesStreamEvent::ContentBlockStop { index: event.index })
        }
        "message_delta" => {
            let event: MessageDeltaEvent =
                serde_json::from_value(data).whatever_context("decode message_delta event")?;
            Ok(MessagesStreamEvent::MessageDelta {
                delta: event.delta,
                usage: event.usage,
            })
        }
        "message_stop" => Ok(MessagesStreamEvent::MessageStop),
        "ping" => Ok(MessagesStreamEvent::Ping),
        "error" => {
            let event: StreamErrorEvent =
                serde_json::from_value(data).whatever_context("decode error event")?;
            Ok(MessagesStreamEvent::Error { error: event.error })
        }
        _ => Ok(MessagesStreamEvent::Unknown {
            event: event_type,
            data,
        }),
    }
}

#[derive(Debug, Deserialize)]
struct MessageStartEvent {
    message: MessagesResponse,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStartEvent {
    index: usize,
    content_block: Block,
}

#[derive(Debug, Deserialize)]
struct ContentBlockDeltaEvent {
    index: usize,
    delta: ContentBlockDelta,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStopEvent {
    index: usize,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaEvent {
    delta: MessageDelta,
    #[serde(default)]
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamErrorEvent {
    error: StreamErrorDetail,
}

pub async fn messages_stream(
    client: &reqwest::Client,
    base_url: &Url,
    mut req: MessagesRequest,
) -> Result<MessagesStream, Whatever> {
    let url = base_url
        .join("v1/messages")
        .whatever_context("join url error")?;
    req.stream = Some(true);
    let req = serde_json::to_string(&req).whatever_context("encode request error")?;
    let resp = client
        .post(url)
        .body(req.clone())
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .send()
        .await
        .whatever_context("send request error")?;

    let status = resp.status();
    trace!(req, ?status, "messages stream API invoked");

    if !status.is_success() {
        let resp = resp.text().await.whatever_context("read response error")?;
        let message = format_error_message(status, &resp);
        return Err(<Whatever as snafu::FromString>::without_source(message));
    }

    Ok(MessagesStream::new(resp))
}

pub async fn messages(
    client: &reqwest::Client,
    base_url: &Url,
    req: MessagesRequest,
) -> Result<MessagesResponse, Whatever> {
    let url = base_url
        .join("v1/messages")
        .whatever_context("join url error")?;
    let req = serde_json::to_string(&req).whatever_context("encode request error")?;
    let resp = client
        .post(url)
        .body(req.clone())
        .header("Content-Type", "application/json")
        .send()
        .await
        .whatever_context("send request error")?;

    let status = resp.status();
    let resp = resp.text().await.whatever_context("read response error")?;
    trace!(req, resp, ?status, "messages API invoked");

    if !status.is_success() {
        let message = format_error_message(status, &resp);
        return Err(<Whatever as snafu::FromString>::without_source(message));
    }

    serde_json::from_str(&resp).whatever_context("decode response error")
}

fn format_error_message(status: StatusCode, body: &str) -> String {
    match serde_json::from_str::<ErrorResponse>(body) {
        Ok(parsed) => {
            let mut message = format!(
                "request failed with status {status}: {}",
                parsed.error.message
            );
            if let Some(code) = parsed.error.code.as_deref() {
                message.push_str(&format!(" (code: {code})"));
            }
            if let Some(kind) = parsed.error.r#type.as_deref() {
                message.push_str(&format!(" (type: {kind})"));
            }
            message
        }
        Err(_) => format!("request failed with status {status}: {body}"),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::{TryStreamExt, stream};
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_choice_serialization() {
        let cases = [
            (
                ToolChoice::none(),
                json!({
                    "type": "none"
                }),
            ),
            (
                ToolChoice::auto().call(),
                json!({
                    "type": "auto"
                }),
            ),
            (
                ToolChoice::any().call(),
                json!({
                    "type": "any"
                }),
            ),
            (
                ToolChoice::any().disable_parallel_tool_use(false).call(),
                json!({
                    "type": "any",
                    "disable_parallel_tool_use": false
                }),
            ),
            (
                ToolChoice::any().disable_parallel_tool_use(true).call(),
                json!({
                    "type": "any",
                    "disable_parallel_tool_use": true
                }),
            ),
            (
                ToolChoice::tool().name("get_weather").call(),
                json!({
                    "type": "tool",
                    "name": "get_weather"
                }),
            ),
        ];
        for (target, expect) in cases {
            let rv = serde_json::to_value(target.clone()).unwrap();
            assert_eq!(rv, expect, "original target: {target:?}")
        }
    }

    #[test]
    fn thinking_serialization() {
        let thinking = Thinking::enabled(1024);
        let rv = serde_json::to_value(thinking).unwrap();
        assert_eq!(
            rv,
            json!({
                "type": "enabled",
                "budget_tokens": 1024
            })
        );
    }

    #[tokio::test]
    async fn stream_parses_events() {
        let payload = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mid = payload.len() / 2;
        let chunks = vec![
            Ok::<Bytes, reqwest::Error>(Bytes::copy_from_slice(&payload.as_bytes()[..mid])),
            Ok::<Bytes, reqwest::Error>(Bytes::copy_from_slice(&payload.as_bytes()[mid..])),
        ];
        let stream = MessagesStream::from_bytes_stream(stream::iter(chunks));
        let events: Vec<_> = stream.try_collect().await.expect("stream should parse");

        assert_eq!(events.len(), 6);
        assert!(matches!(
            events[0],
            MessagesStreamEvent::MessageStart { .. }
        ));
        assert!(matches!(
            events[1],
            MessagesStreamEvent::ContentBlockStart { index: 0, .. }
        ));
        match &events[2] {
            MessagesStreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(*index, 0);
                match delta {
                    ContentBlockDelta::TextDelta { text } => assert_eq!(text, "Hello"),
                    other => panic!("unexpected delta: {other:?}"),
                }
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(matches!(
            events[3],
            MessagesStreamEvent::ContentBlockStop { index: 0 }
        ));
        match &events[4] {
            MessagesStreamEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason, Some(StopReason::EndTurn));
                assert_eq!(usage.as_ref().and_then(|u| u.output_tokens), Some(5));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(matches!(events[5], MessagesStreamEvent::MessageStop));
    }

    mod networking {
        use snafu::Whatever;

        use crate::{
            Message, MessagesRequest,
            client::{messages::messages, tests::*},
        };

        #[tokio::test]
        #[snafu::report]
        async fn simple_messages() -> Result<(), Whatever> {
            let cli = test_client();
            let TestMetadata {
                base_url, model, ..
            } = test_md();

            let msgs = vec![Message::user("Hello!".into())];
            let resp = messages(
                cli,
                &base_url,
                MessagesRequest::builder()
                    .messages(msgs)
                    .model(&model)
                    .build(),
            )
            .await?;
            println!("{resp:?}");

            Ok(())
        }
    }
}
