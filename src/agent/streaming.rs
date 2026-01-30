//! Streaming support for agent chat interactions.
//!
//! This module provides types and utilities for handling streaming responses
//! from AI providers, including event accumulation, retry logic, and progress updates.

use std::collections::HashMap;
use std::time::Duration;

use crate::provider::{Block, ContentBlockDelta, MessagesStreamEvent, StopReason, UsageStats};
use crate::{RetryAttempt as CoreRetryAttempt, RetryUpdate, StreamError};
use snafu::ResultExt;
use tokio_util::sync::CancellationToken;

use super::{ChatStreamUpdate, RequestOptions};

const STREAM_RETRY_BASE_DELAY_MS: u64 = 200;

/// Action to take after handling a stream event.
pub enum StreamAction {
    Continue,
    Stop,
}

/// Accumulates streaming events into a complete response.
pub struct StreamAccumulator {
    blocks: Vec<Block>,
    tool_inputs: HashMap<usize, String>,
    stop_reason: Option<StopReason>,
    usage: Option<UsageStats>,
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            tool_inputs: HashMap::new(),
            stop_reason: None,
            usage: None,
        }
    }

    pub fn finish(self) -> (Vec<Block>, Option<StopReason>, Option<UsageStats>) {
        (self.blocks, self.stop_reason, self.usage)
    }

    /// Handle a stream event and return the next action.
    pub fn handle_event<F>(
        &mut self,
        event: MessagesStreamEvent,
        on_update: &mut F,
    ) -> Result<StreamAction, crate::Error>
    where
        F: FnMut(ChatStreamUpdate),
    {
        match event {
            MessagesStreamEvent::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    self.update_usage(usage);
                }
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                self.ensure_block_slot(index);
                self.blocks[index] = content_block;
                match &self.blocks[index] {
                    Block::Text { text } if !text.is_empty() => {
                        on_update(ChatStreamUpdate::Plain {
                            index,
                            text: text.clone(),
                        });
                    }
                    Block::Thinking { thinking, .. } if !thinking.is_empty() => {
                        on_update(ChatStreamUpdate::Thinking {
                            index,
                            text: thinking.clone(),
                        });
                    }
                    _ => (),
                }
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::ContentBlockDelta { index, delta } => {
                self.apply_delta(index, delta, on_update)?;
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::ContentBlockStop { index } => {
                self.finalize_tool_input(index)?;
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::MessageDelta { delta, usage } => {
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
                if let Some(usage) = usage {
                    self.update_usage(usage);
                }
                Ok(StreamAction::Continue)
            }
            MessagesStreamEvent::MessageStop => Ok(StreamAction::Stop),
            MessagesStreamEvent::Ping => Ok(StreamAction::Continue),
            MessagesStreamEvent::Error { error } => {
                let mut message = format!("stream error: {}", error.message);
                if let Some(code) = error.code.as_deref() {
                    message.push_str(&format!(" (code: {code})"));
                }
                if let Some(kind) = error.r#type.as_deref() {
                    message.push_str(&format!(" (type: {kind})"));
                }
                snafu::whatever!("{message}")
            }
            MessagesStreamEvent::Unknown { .. } => Ok(StreamAction::Continue),
        }
    }

    fn update_usage(&mut self, usage: UsageStats) {
        match &mut self.usage {
            Some(current) => current.merge(usage),
            None => {
                let mut current = usage;
                if current.total_tokens.is_none()
                    && let (Some(input), Some(output)) =
                        (current.input_tokens, current.output_tokens)
                {
                    current.total_tokens = Some(input + output);
                }
                self.usage = Some(current);
            }
        }
    }

    fn ensure_block_slot(&mut self, index: usize) {
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, || Block::Text {
                text: String::new(),
            });
        }
    }

    fn apply_delta<F>(
        &mut self,
        index: usize,
        delta: ContentBlockDelta,
        on_update: &mut F,
    ) -> Result<(), crate::Error>
    where
        F: FnMut(ChatStreamUpdate),
    {
        self.ensure_block_slot(index);
        match delta {
            ContentBlockDelta::TextDelta { text } => {
                if text.is_empty() {
                    return Ok(());
                }
                match &mut self.blocks[index] {
                    Block::Text { text: current } => current.push_str(&text),
                    _ => {
                        self.blocks[index] = Block::Text { text: text.clone() };
                    }
                }
                on_update(ChatStreamUpdate::Plain { index, text });
            }
            ContentBlockDelta::ThinkingDelta { thinking } => {
                if thinking.is_empty() {
                    return Ok(());
                }
                match &mut self.blocks[index] {
                    Block::Thinking {
                        thinking: current, ..
                    } => current.push_str(&thinking),
                    _ => {
                        self.blocks[index] = Block::Thinking {
                            thinking: thinking.clone(),
                            signature: None,
                        };
                    }
                }
                on_update(ChatStreamUpdate::Thinking {
                    index,
                    text: thinking,
                });
            }
            ContentBlockDelta::SignatureDelta { signature } => {
                if let Block::Thinking {
                    signature: slot, ..
                } = &mut self.blocks[index]
                {
                    *slot = Some(signature);
                }
            }
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                if !partial_json.is_empty() {
                    self.tool_inputs
                        .entry(index)
                        .or_default()
                        .push_str(&partial_json);
                }
            }
            ContentBlockDelta::Unknown => (),
        }
        Ok(())
    }

    fn finalize_tool_input(&mut self, index: usize) -> Result<(), crate::Error> {
        let Some(buffer) = self.tool_inputs.remove(&index) else {
            return Ok(());
        };
        if buffer.is_empty() {
            return Ok(());
        }
        let input: serde_json::Value =
            serde_json::from_str(&buffer).whatever_context("decode tool input json")?;
        if let Some(Block::ToolUse(tool_use)) = self.blocks.get_mut(index) {
            tool_use.input = input;
        }
        Ok(())
    }
}

/// Calculate the delay for stream retry based on attempt number.
pub fn stream_retry_delay(attempt: usize, max_delay: Duration) -> Duration {
    let shift = attempt.saturating_sub(1).min(30) as u32;
    let multiplier = 1u64 << shift;
    let delay_ms = STREAM_RETRY_BASE_DELAY_MS.saturating_mul(multiplier);
    let mut delay = Duration::from_millis(delay_ms);
    if max_delay != Duration::from_millis(0) && delay > max_delay {
        delay = max_delay;
    }
    delay
}

/// Notify retry attempt for stream.
pub fn notify_stream_retry_attempt(
    request_options: &RequestOptions,
    attempt: usize,
    delay: Duration,
    err: &StreamError,
) {
    if let Some(notifier) = &request_options.retry_notifier {
        notifier.notify(RetryUpdate::Attempt(CoreRetryAttempt {
            attempt,
            max_attempts: request_options.retry_max_attempts,
            delay,
            error: err.to_string(),
        }));
    }
}

/// Notify retry finished for stream.
pub fn notify_stream_retry_finished(request_options: &RequestOptions, success: bool) {
    if let Some(notifier) = &request_options.retry_notifier {
        notifier.notify(RetryUpdate::Finished { success });
    }
}

/// Wait for retry delay or cancellation.
pub async fn wait_for_retry(delay: Duration, cancel_token: &CancellationToken) -> bool {
    if delay == Duration::from_millis(0) {
        return true;
    }
    tokio::select! {
        _ = cancel_token.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider;

    #[test]
    fn stream_accumulator_updates_plain_and_thinking() {
        let mut accumulator = StreamAccumulator::new();
        let mut updates = Vec::new();
        let mut on_update = |update| updates.push(update);

        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: Block::text(""),
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta {
                        text: "Hello".to_string(),
                    },
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockStart {
                    index: 1,
                    content_block: Block::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::ContentBlockDelta {
                    index: 1,
                    delta: ContentBlockDelta::ThinkingDelta {
                        thinking: "Reasoning".to_string(),
                    },
                },
                &mut on_update,
            )
            .unwrap();
        accumulator
            .handle_event(
                MessagesStreamEvent::MessageDelta {
                    delta: provider::MessageDelta {
                        stop_reason: Some(StopReason::EndTurn),
                        stop_sequence: None,
                    },
                    usage: None,
                },
                &mut on_update,
            )
            .unwrap();
        let action = accumulator
            .handle_event(MessagesStreamEvent::MessageStop, &mut on_update)
            .unwrap();

        assert!(matches!(action, StreamAction::Stop));
        assert_eq!(updates.len(), 2);
        match &updates[0] {
            ChatStreamUpdate::Plain { index, text } => {
                assert_eq!(*index, 0);
                assert_eq!(text, "Hello");
            }
            other => panic!("unexpected update: {other:?}"),
        }
        match &updates[1] {
            ChatStreamUpdate::Thinking { index, text } => {
                assert_eq!(*index, 1);
                assert_eq!(text, "Reasoning");
            }
            other => panic!("unexpected update: {other:?}"),
        }

        let (blocks, stop_reason, _) = accumulator.finish();
        assert_eq!(stop_reason, Some(StopReason::EndTurn));
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Text { text } => assert_eq!(text, "Hello"),
            other => panic!("unexpected block: {other:?}"),
        }
        match &blocks[1] {
            Block::Thinking { thinking, .. } => assert_eq!(thinking, "Reasoning"),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn retry_stream_error_heuristics() {
        let err = StreamError::transport("read stream chunk error: broken pipe".to_string());
        assert!(err.is_retryable());

        let err = StreamError::decode("decode stream event data".to_string());
        assert!(!err.is_retryable());

        let err = StreamError::decode("chat stream cancelled".to_string());
        assert!(!err.is_retryable());
    }
}
