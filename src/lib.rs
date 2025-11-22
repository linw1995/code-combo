mod agent;
mod combo;
mod config;
mod error;
mod text_edit;
pub mod tools;

pub use agent::*;
pub use combo::*;
pub use config::*;
pub use error::*;
pub use text_edit::*;

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
        .with(EnvFilter::from_default_env())
        .init();
}
