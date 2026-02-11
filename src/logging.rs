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

pub(crate) fn sanitize_log_stem(stem: &str) -> String {
    let trimmed = stem.trim();
    if trimmed.is_empty() {
        return String::new();
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

    let file_stem = sanitize_log_stem(log_name);
    let file_stem = if file_stem.is_empty() {
        "coco".to_string()
    } else {
        file_stem
    };
    let file_name = ensure_log_extension(&file_stem);
    let log_path = logs_dir.join(file_name);

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .whatever_context(format!("failed to open log file {}", log_path.display()))?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::INFO.into())
        .with_env_var("COCO_LOG")
        .from_env()
        .whatever_context("failed to load filter from env")?;

    let file_layer = fmt::layer()
        .json()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
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

#[cfg(test)]
mod tests {
    use super::sanitize_log_stem;

    #[test]
    fn sanitize_log_stem_replaces_unsupported_characters() {
        assert_eq!(sanitize_log_stem(" mcp/server:dev "), "mcp_server_dev");
    }

    #[test]
    fn sanitize_log_stem_trims_edge_separators() {
        assert_eq!(sanitize_log_stem(".__-name-__."), "name");
        assert_eq!(sanitize_log_stem("   "), "");
    }
}
