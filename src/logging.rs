use std::{
    fs,
    path::{Path, PathBuf},
};

use snafu::prelude::*;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::Result;

fn default_logs_dir() -> PathBuf {
    PathBuf::from(".coco").join("logs")
}

fn sanitize_file_stem(stem: &str) -> String {
    let trimmed = stem.trim();
    if trimmed.is_empty() {
        return "coco".to_string();
    }

    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => out.push(ch),
            _ => out.push('_'),
        }
    }

    out.trim_matches(['.', '-', '_']).to_string()
}

fn ensure_log_extension(name: &str) -> String {
    if name.ends_with(".log") {
        name.to_string()
    } else {
        format!("{name}.log")
    }
}

pub fn init_file_logging(log_name: &str) -> Result<PathBuf> {
    let logs_dir = default_logs_dir();
    fs::create_dir_all(&logs_dir)
        .whatever_context(format!("failed to create logs dir {}", logs_dir.display()))?;

    let file_stem = sanitize_file_stem(log_name);
    let file_name = ensure_log_extension(&file_stem);
    let log_path = logs_dir.join(file_name);

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .whatever_context(format!("failed to open log file {}", log_path.display()))?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::INFO.into())
        .from_env()
        .whatever_context("failed to load filter from env")?;

    let file_layer = fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
        .pretty()
        .with_filter(env_filter);

    let subscriber = tracing_subscriber::registry().with(file_layer);
    match tracing::dispatcher::set_global_default(tracing::Dispatch::new(subscriber)) {
        Ok(()) => Ok(log_path),
        Err(_) => Ok(log_path),
    }
}

pub fn init_file_logging_best_effort(log_name: &str) -> Option<PathBuf> {
    init_file_logging(log_name).ok()
}

pub fn logs_dir() -> &'static Path {
    Path::new(".coco/logs")
}
