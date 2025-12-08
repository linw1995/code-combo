use std::path::Path;

use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tracing::warn;

use super::Session;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentSessionMetadata {
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

impl PersistentSessionMetadata {
    pub fn filename(&self) -> String {
        format!("{}.json", self.created_at.unix_timestamp_nanos())
    }

    pub fn metadata_filename(&self) -> String {
        format!("{}.metadata.json", self.created_at.unix_timestamp_nanos())
    }
}

#[derive(Serialize, Deserialize)]
pub struct PersistentSession {
    pub name: String,
    pub inner: Session,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

impl PersistentSession {
    pub fn filename(&self) -> String {
        format!("{}.json", self.created_at.unix_timestamp_nanos())
    }

    pub fn to_metadata(&self) -> PersistentSessionMetadata {
        PersistentSessionMetadata {
            name: self.name.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

async fn load_session_metadata(
    session_dir: &Path,
    filename: &str,
) -> Result<PersistentSessionMetadata> {
    let file_path = session_dir.join(filename);
    let json = tokio::fs::read_to_string(&file_path)
        .await
        .whatever_context("failed to read session metadata from file")?;

    let metadata: PersistentSessionMetadata =
        serde_json::from_str(&json).whatever_context("failed to deserialize session metadata")?;

    Ok(metadata)
}

pub async fn list_session(session_dir: &Path) -> Result<Vec<PersistentSessionMetadata>> {
    let mut sessions = Vec::new();

    let mut entries = tokio::fs::read_dir(session_dir)
        .await
        .whatever_context("failed to read session directory")?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .whatever_context("failed to read directory entry")?
    {
        let file_path = entry.path();

        if !file_path.is_file() {
            continue;
        }

        if let Some(extension) = file_path.extension().and_then(|e| e.to_str()) {
            if extension != "json" {
                continue;
            }

            let file_name = file_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            if !file_name.contains("metadata") {
                continue;
            }

            if let Some(file_path_str) = file_path.to_str() {
                match load_session_metadata(session_dir, &file_name).await {
                    Ok(metadata) => sessions.push(metadata),
                    Err(err) => {
                        warn!(
                            ?err,
                            path = file_path_str,
                            "failed to load session metadata"
                        );
                        continue;
                    }
                }
            }
        }
    }

    Ok(sessions)
}

pub async fn save_session(session_dir: &Path, session: PersistentSession) -> Result<()> {
    // Save full session
    let json =
        serde_json::to_string_pretty(&session).whatever_context("failed to serialize session")?;

    let session_path = session_dir.join(session.filename());
    tokio::fs::write(&session_path, json)
        .await
        .whatever_context("failed to write session to file")?;

    // Save metadata
    let metadata = session.to_metadata();
    let metadata_json =
        serde_json::to_string_pretty(&metadata).whatever_context("failed to serialize metadata")?;

    let metadata_path = session_dir.join(metadata.metadata_filename());
    tokio::fs::write(&metadata_path, metadata_json)
        .await
        .whatever_context("failed to write metadata to file")?;

    Ok(())
}

pub async fn load_session(session_dir: &Path, filename: &str) -> Result<PersistentSession> {
    let file_path = session_dir.join(filename);

    let json = tokio::fs::read_to_string(&file_path)
        .await
        .whatever_context("failed to read session from file")?;

    let session: PersistentSession =
        serde_json::from_str(&json).whatever_context("failed to deserialize session")?;

    Ok(session)
}
