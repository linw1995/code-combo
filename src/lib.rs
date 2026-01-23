#![recursion_limit = "1024"]

mod agent;
pub mod cli;
pub mod cmd;
mod combo;
mod config;
mod error;
pub mod exec;
pub mod global;
pub mod logging;
mod mcp;
mod provider;
mod retry;
mod runtime_overrides;
mod stream_error;
mod text_edit;
pub mod tools;
pub mod version;

pub use agent::*;
pub use combo::*;
pub use config::*;
pub use error::*;
pub use exec::*;
pub use mcp::*;
pub use provider::*;
pub use retry::*;
pub use runtime_overrides::*;
pub use stream_error::*;
pub use text_edit::*;

#[cfg(test)]
mod test_utils;

#[cfg(test)]
#[ctor::ctor]
fn init() {
    use std::io;
    use tracing_subscriber::{EnvFilter, prelude::*};

    let console_log = tracing_subscriber::fmt::layer()
        .pretty()
        .with_writer(io::stdout)
        .boxed();

    tracing_subscriber::registry()
        .with(vec![console_log])
        .with(EnvFilter::from_env("COCO_LOG"))
        .init();
}
