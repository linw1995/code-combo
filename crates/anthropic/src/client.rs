mod messages;

use bon::bon;
pub use messages::*;
use reqwest::{Url, header::HeaderMap};
use snafu::{ResultExt, Whatever};

use crate::Message;

pub struct Client {
    model: String,
    base_url: Url,
    cli: reqwest::Client,
}

type Result<T> = std::result::Result<T, Whatever>;

#[bon]
impl Client {
    #[builder]
    pub fn new(base_url: &str, token: &str, model: &str) -> Result<Self> {
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
        Ok(Self {
            model: model.to_string(),
            base_url: base_url.parse().whatever_context("parse base url error")?,
            cli: reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .whatever_context("build client error")?,
        })
    }

    pub async fn messages(&self, msgs: Vec<Message>) -> Result<MessagesResponse> {
        messages::messages(
            &self.cli,
            &self.base_url,
            MessagesRequest::simple(&self.model, msgs),
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
}
