use snafu::prelude::*;
use tracing_error::ErrorLayer;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::error::*;

pub fn init() -> Result<()> {
    let log_path = "coco-tui.log";
    let log_file = std::fs::File::create(log_path).whatever_context("failed to create log file")?;
    let env_filter = EnvFilter::builder().with_default_directive(tracing::Level::INFO.into());
    let env_filter = env_filter
        .from_env()
        .whatever_context("failed to load filter from env")?;
    let file_subscriber = fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
        .with_filter(env_filter);
    tracing_subscriber::registry()
        .with(file_subscriber)
        .with(ErrorLayer::default())
        .try_init()
        .whatever_context("failed to init traceing subscriber")?;
    Ok(())
}
