pub mod actions;
pub mod app;
pub mod global;
#[macro_use]
pub mod components;
pub mod events;
pub mod logging;

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

    // Initialize dummy channels for testing.
    use tokio::sync::mpsc;
    let (event_tx, _) = mpsc::unbounded_channel();
    let (action_tx, _) = mpsc::unbounded_channel();
    global::initialize(event_tx, action_tx);
}
