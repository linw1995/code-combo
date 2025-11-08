use bon::bon;
use reqwest::Url;
use serde::{Deserialize, Serialize};
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
    /// Definitions of tools that the model may use.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

#[bon]
impl MessagesRequest {
    #[builder]
    pub fn new(model: &str, messages: Vec<Message>, #[builder(default)] tools: Vec<Tool>) -> Self {
        Self {
            model: model.to_string(),
            messages,
            max_tokens: 1024,
            temperature: None,
            tool_choice: None,
            tools,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
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

pub async fn messages(
    client: &reqwest::Client,
    base_url: &Url,
    req: MessagesRequest,
) -> Result<MessagesResponse, Whatever> {
    let url = base_url
        .join("v1/messages")
        .whatever_context("join url error")?;
    let resp = client
        .post(url)
        .json(&req)
        .send()
        .await
        .whatever_context("send request error")?;

    let resp = resp.text().await.whatever_context("read response error")?;
    trace!(?req, resp, "messages API invoked");

    serde_json::from_str(&resp).whatever_context("decode response error")
}

#[cfg(test)]
mod tests {
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
