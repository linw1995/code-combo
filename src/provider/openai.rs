use std::{
    collections::{HashMap, HashSet, VecDeque},
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;
use serde_json::Value;
use snafu::Whatever;

use ::openai as openai_api;

use crate::provider::types::{
    Block, Content, ContentBlockDelta, Message, MessageDelta, MessagesResponse,
    MessagesStreamEvent, Role, StopReason, StreamErrorDetail, Thinking, Tool, ToolChoice, ToolUse,
};

struct ToolCallState {
    #[allow(dead_code)]
    id: String,
    name: String,
}

#[allow(dead_code)]
pub fn build_client(
    base_url: &str,
    token: &str,
    model: &str,
    user_agent: Option<String>,
) -> Result<openai_api::Client, Whatever> {
    openai_api::Client::new(base_url, token, model, user_agent)
}

pub async fn messages(
    client: &openai_api::Client,
    system_prompt: Option<&str>,
    conversations: Vec<Message>,
    tools: Vec<Tool>,
    tool_choice: Option<ToolChoice>,
    thinking: Option<Thinking>,
) -> Result<MessagesResponse, Whatever> {
    let request = build_request(system_prompt, conversations, tools, tool_choice, thinking)?;
    let response = client.chat_completions(request).await?;
    Ok(response_into_messages(response))
}

pub async fn messages_stream(
    client: &openai_api::Client,
    system_prompt: Option<&str>,
    conversations: Vec<Message>,
    tools: Vec<Tool>,
    tool_choice: Option<ToolChoice>,
    thinking: Option<Thinking>,
) -> Result<OpenAIStream, Whatever> {
    let request = build_request(system_prompt, conversations, tools, tool_choice, thinking)?;
    let stream = client.chat_completions_stream(request).await?;
    Ok(OpenAIStream::new(stream))
}

fn build_request(
    system_prompt: Option<&str>,
    conversations: Vec<Message>,
    tools: Vec<Tool>,
    tool_choice: Option<ToolChoice>,
    thinking: Option<Thinking>,
) -> Result<openai_api::ChatCompletionRequest, Whatever> {
    let mut messages = Vec::new();
    if let Some(system_prompt) = system_prompt
        && !system_prompt.trim().is_empty()
    {
        messages.push(openai_api::ChatMessage {
            role: openai_api::Role::System,
            content: Some(system_prompt.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    messages.extend(convert_messages(conversations)?);
    let tools = tools
        .into_iter()
        .map(|tool| openai_api::Tool {
            r#type: "function".to_string(),
            function: openai_api::FunctionDefinition {
                name: tool.name,
                description: Some(tool.description),
                parameters: Some(tool.input_schema),
            },
        })
        .collect();
    let tool_choice = tool_choice.map(convert_tool_choice);
    let (reasoning_effort, max_completion_tokens) = match thinking {
        Some(Thinking::Enabled { budget_tokens }) => {
            (Some(openai_api::ReasoningEffort::High), Some(budget_tokens))
        }
        None => (None, None),
    };
    Ok(openai_api::ChatCompletionRequest {
        model: String::new(),
        messages,
        tools,
        tool_choice,
        stream: None,
        stream_options: None,
        reasoning_effort,
        max_completion_tokens,
    })
}

fn convert_tool_choice(choice: ToolChoice) -> openai_api::ToolChoice {
    match choice {
        ToolChoice::None => openai_api::ToolChoice::String("none".to_string()),
        ToolChoice::Auto { .. } => openai_api::ToolChoice::String("auto".to_string()),
        ToolChoice::Any { .. } => openai_api::ToolChoice::String("required".to_string()),
        ToolChoice::Tool { name, .. } => openai_api::ToolChoice::Function {
            r#type: "function".to_string(),
            function: openai_api::FunctionChoice { name },
        },
    }
}

fn convert_messages(conversations: Vec<Message>) -> Result<Vec<openai_api::ChatMessage>, Whatever> {
    let mut output = Vec::new();
    for message in conversations {
        match message.role {
            Role::User => {
                convert_user_message(message.content, &mut output)?;
            }
            Role::Assistant => {
                convert_assistant_message(message.content, &mut output)?;
            }
        }
    }
    Ok(output)
}

fn convert_user_message(
    content: Content,
    output: &mut Vec<openai_api::ChatMessage>,
) -> Result<(), Whatever> {
    let mut text = String::new();
    let mut tool_results = Vec::new();
    match content {
        Content::Text(value) => {
            if !value.is_empty() {
                output.push(openai_api::ChatMessage {
                    role: openai_api::Role::User,
                    content: Some(value),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            return Ok(());
        }
        Content::Multiple(blocks) => {
            for block in blocks {
                match block {
                    Block::Text { text: block_text } => text.push_str(&block_text),
                    Block::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => tool_results.push((tool_use_id, content)),
                    Block::Thinking { .. } => (),
                    Block::ToolUse(_) => (),
                }
            }
        }
    }
    if !text.is_empty() {
        output.push(openai_api::ChatMessage {
            role: openai_api::Role::User,
            content: Some(text),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    for (tool_use_id, result_content) in tool_results {
        let content = content_to_text(&result_content);
        output.push(openai_api::ChatMessage {
            role: openai_api::Role::Tool,
            content: Some(content),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_use_id),
            name: None,
        });
    }
    Ok(())
}

fn convert_assistant_message(
    content: Content,
    output: &mut Vec<openai_api::ChatMessage>,
) -> Result<(), Whatever> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    match content {
        Content::Text(value) => {
            if !value.is_empty() {
                output.push(openai_api::ChatMessage {
                    role: openai_api::Role::Assistant,
                    content: Some(value),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            return Ok(());
        }
        Content::Multiple(blocks) => {
            for block in blocks {
                match block {
                    Block::Text { text: block_text } => text.push_str(&block_text),
                    Block::ToolUse(tool_use) => {
                        let arguments = serde_json::to_string(&tool_use.input)
                            .unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(openai_api::ToolCall {
                            id: tool_use.id,
                            r#type: "function".to_string(),
                            function: openai_api::FunctionCall {
                                name: tool_use.name,
                                arguments,
                            },
                        });
                    }
                    Block::Thinking { .. } => (),
                    Block::ToolResult { .. } => (),
                }
            }
        }
    }
    if !text.is_empty() || !tool_calls.is_empty() {
        output.push(openai_api::ChatMessage {
            role: openai_api::Role::Assistant,
            content: if text.is_empty() { None } else { Some(text) },
            reasoning_content: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
        });
    }
    Ok(())
}

fn content_to_text(content: &Content) -> String {
    match content {
        Content::Text(text) => text.clone(),
        Content::Multiple(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

fn response_into_messages(response: openai_api::ChatCompletionResponse) -> MessagesResponse {
    let mut content_blocks = Vec::new();
    let mut stop_reason = None;
    let stop_sequence = None;
    if let Some(choice) = response.choices.into_iter().next() {
        stop_reason = choice.finish_reason.and_then(map_finish_reason);
        if let Some(reasoning_content) = choice.message.reasoning_content
            && !reasoning_content.is_empty()
        {
            content_blocks.push(Block::Thinking {
                thinking: reasoning_content,
                signature: None,
            });
        }
        if let Some(content) = choice.message.content
            && !content.is_empty()
        {
            content_blocks.push(Block::Text { text: content });
        }
        if let Some(tool_calls) = choice.message.tool_calls {
            for tool_call in tool_calls {
                let input = parse_tool_arguments(&tool_call.function.arguments);
                content_blocks.push(Block::ToolUse(ToolUse {
                    id: tool_call.id,
                    name: tool_call.function.name,
                    input,
                }));
            }
        }
    }
    MessagesResponse {
        content: content_blocks,
        stop_reason,
        stop_sequence,
    }
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_string()))
}

fn map_finish_reason(reason: String) -> Option<StopReason> {
    match reason.as_str() {
        "stop" => Some(StopReason::EndTurn),
        "length" => Some(StopReason::MaxTokens),
        "tool_calls" | "function_call" => Some(StopReason::ToolUse),
        "content_filter" => Some(StopReason::Refusal),
        _ => None,
    }
}

pub struct OpenAIStream {
    inner: openai_api::ChatCompletionStream,
    pending: VecDeque<MessagesStreamEvent>,
    thinking_started: bool,
    thinking_closed: bool,
    text_started: bool,
    text_closed: bool,
    tool_started: HashMap<usize, ToolCallState>,
    tool_closed: HashSet<usize>,
    finished: bool,
}

impl OpenAIStream {
    fn new(inner: openai_api::ChatCompletionStream) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(MessagesStreamEvent::MessageStart {
            message: MessagesResponse {
                content: Vec::new(),
                stop_reason: None,
                stop_sequence: None,
            },
        });
        Self {
            inner,
            pending,
            thinking_started: false,
            thinking_closed: false,
            text_started: false,
            text_closed: false,
            tool_started: HashMap::new(),
            tool_closed: HashSet::new(),
            finished: false,
        }
    }

    fn push_event(&mut self, event: MessagesStreamEvent) {
        self.pending.push_back(event);
    }

    fn push_stop_reason(&mut self, stop_reason: Option<StopReason>) {
        if let Some(stop_reason) = stop_reason {
            self.push_event(MessagesStreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(stop_reason),
                    stop_sequence: None,
                },
                usage: None,
            });
        }
        self.push_event(MessagesStreamEvent::MessageStop);
    }

    fn close_thinking_block(&mut self) {
        if self.thinking_started && !self.thinking_closed {
            self.push_event(MessagesStreamEvent::ContentBlockStop { index: 0 });
            self.thinking_closed = true;
        }
    }

    fn tool_base_index(&self) -> usize {
        (self.thinking_started as usize) + (self.text_started as usize)
    }

    fn close_all_blocks(&mut self) {
        self.close_thinking_block();
        if self.text_started && !self.text_closed {
            self.push_event(MessagesStreamEvent::ContentBlockStop {
                index: if self.thinking_started { 1 } else { 0 },
            });
            self.text_closed = true;
        }
        for index in self.tool_started.keys().cloned().collect::<Vec<_>>() {
            if !self.tool_closed.contains(&index) {
                self.push_event(MessagesStreamEvent::ContentBlockStop { index });
                self.tool_closed.insert(index);
            }
        }
    }
}

impl Stream for OpenAIStream {
    type Item = Result<MessagesStreamEvent, Whatever>;

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
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Some(Err(err))),
                Poll::Ready(Some(Ok(chunk))) => {
                    if let Some(choice) = chunk.choices.into_iter().next() {
                        let delta = choice.delta;
                        if let Some(reasoning_content) = delta.reasoning_content
                            && !reasoning_content.is_empty()
                        {
                            if !this.thinking_started {
                                this.thinking_started = true;
                                this.push_event(MessagesStreamEvent::ContentBlockStart {
                                    index: 0,
                                    content_block: Block::Thinking {
                                        thinking: String::new(),
                                        signature: None,
                                    },
                                });
                            }
                            this.push_event(MessagesStreamEvent::ContentBlockDelta {
                                index: 0,
                                delta: ContentBlockDelta::ThinkingDelta {
                                    thinking: reasoning_content,
                                },
                            });
                        }
                        if let Some(content) = delta.content
                            && !content.is_empty()
                        {
                            this.close_thinking_block();
                            let text_index = if this.thinking_started { 1 } else { 0 };
                            if !this.text_started {
                                this.text_started = true;
                                this.push_event(MessagesStreamEvent::ContentBlockStart {
                                    index: text_index,
                                    content_block: Block::Text {
                                        text: String::new(),
                                    },
                                });
                            }
                            this.push_event(MessagesStreamEvent::ContentBlockDelta {
                                index: text_index,
                                delta: ContentBlockDelta::TextDelta { text: content },
                            });
                        }
                        if let Some(tool_calls) = delta.tool_calls {
                            let base_index = this.tool_base_index();
                            for tool_call in tool_calls {
                                let index = base_index + tool_call.index;
                                if !this.tool_started.contains_key(&index) {
                                    let id = tool_call
                                        .id
                                        .clone()
                                        .unwrap_or_else(|| format!("tool_call_{index}"));
                                    let name = tool_call
                                        .function
                                        .as_ref()
                                        .and_then(|f| f.name.clone())
                                        .unwrap_or_else(|| "unknown_tool".to_string());
                                    this.push_event(MessagesStreamEvent::ContentBlockStart {
                                        index,
                                        content_block: Block::ToolUse(ToolUse {
                                            id: id.clone(),
                                            name: name.clone(),
                                            input: Value::Null,
                                        }),
                                    });
                                    this.tool_started.insert(index, ToolCallState { id, name });
                                }
                                let entry = this
                                    .tool_started
                                    .get_mut(&index)
                                    .expect("tool state must exist");
                                if let Some(function) = tool_call.function {
                                    if let Some(name) = function.name
                                        && entry.name == "unknown_tool"
                                    {
                                        entry.name = name;
                                    }
                                    if let Some(arguments) = function.arguments
                                        && !arguments.is_empty()
                                    {
                                        this.push_event(MessagesStreamEvent::ContentBlockDelta {
                                            index,
                                            delta: ContentBlockDelta::InputJsonDelta {
                                                partial_json: arguments,
                                            },
                                        });
                                    }
                                }
                            }
                        }
                        if let Some(reason) = choice.finish_reason {
                            let stop_reason = map_finish_reason(reason);
                            this.close_all_blocks();
                            this.push_stop_reason(stop_reason);
                            this.finished = true;
                        }
                    }
                }
                Poll::Ready(None) => {
                    if !this.finished {
                        this.close_all_blocks();
                        this.push_stop_reason(None);
                    }
                    this.finished = true;
                }
            }
        }
    }
}

impl From<openai_api::ErrorDetail> for StreamErrorDetail {
    fn from(value: openai_api::ErrorDetail) -> Self {
        Self {
            message: value.message,
            r#type: value.r#type,
            code: value.code,
        }
    }
}
