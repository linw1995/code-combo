use std::path::Path;

use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{SessionEnv, Starter, StarterCommand, StarterError};

pub struct DiscoverResult {
    pub starters: Vec<Starter>,
    pub cancelled: bool,
}
pub async fn discover_starters(
    combo_dirs: &[&Path],
    cancel_token: CancellationToken,
) -> DiscoverResult {
    let mut starters = Vec::new();
    let mut cancelled = cancel_token.is_cancelled();
    for combo_dir in combo_dirs {
        let rv = discover_starter_in(combo_dir, cancel_token.clone()).await;
        starters.extend(rv.starters);
        cancelled = rv.cancelled;
    }

    DiscoverResult {
        starters,
        cancelled,
    }
}

async fn discover_starter_in(combo_dir: &Path, cancel_token: CancellationToken) -> DiscoverResult {
    let mut starters = Vec::new();
    let mut entries = match tokio::fs::read_dir(combo_dir).await {
        Ok(entries) => entries,
        Err(err) => {
            warn!(?combo_dir, ?err, "read dir error");
            return DiscoverResult {
                starters,
                cancelled: false,
            };
        }
    };

    loop {
        let entry = tokio::select! {
            _ = cancel_token.cancelled() => {
                return DiscoverResult { starters, cancelled: true };
            }
            entry = entries.next_entry() => entry,
        };
        let entry = match entry {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(err) => {
                warn!(?combo_dir, ?err, "read dir error");
                break;
            }
        };

        let path = entry.path();
        let session_env = SessionEnv::builder()
            .build()
            .expect("failed to build session");
        let starter = match StarterCommand::new(path.to_string_lossy())
            .discovery(true)
            .session_env(session_env)
            .execute()
            .consume_with_cancel(cancel_token.clone(), |_| {})
            .await
        {
            Ok(starter) => starter,
            Err(err) => {
                warn!(?err, "starter join error");
                continue;
            }
        };

        if matches!(&starter.combo, Err(StarterError::Cancelled)) || cancel_token.is_cancelled() {
            return DiscoverResult {
                starters,
                cancelled: true,
            };
        }

        starters.push(starter);
    }

    DiscoverResult {
        starters,
        cancelled: false,
    }
}
