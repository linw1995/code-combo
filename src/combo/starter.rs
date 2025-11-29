use std::process::Stdio;

use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process,
    sync::broadcast,
    task::{self, JoinHandle},
};
use tracing::{debug, error, warn};

use crate::{Combo, parse};

#[derive(Debug, Clone, Snafu)]
pub enum StarterError {
    #[snafu(display("Starter timeout after {seconds}s"))]
    Timeout { seconds: usize },
    #[snafu(display("Combo file is not excutable"))]
    NotExcutable,
    #[snafu(display("Invalid combo: {reason}"))]
    Invalid { reason: String },
}

#[derive(Debug, Clone)]
pub struct Starter {
    pub path: String,
    pub combo: Result<Combo, StarterError>,
}

pub async fn discover_combo_starters(path: &str) -> Vec<Starter> {
    match fs::read_dir(path).await {
        Ok(mut entries) => {
            let mut starters = vec![];
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let (execution, _) = execute_starter(&path.to_string_lossy(), false);
                starters.push(execution.await.expect("execute_starter success"));
            }
            starters
        }
        Err(err) => {
            warn!(?path, ?err, "read dir error");
            Vec::new()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LineContent {
    Stdout(String),
    Stderr(String),
}

impl std::fmt::Display for LineContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineContent::Stdout(text) => f.write_str(text),
            LineContent::Stderr(text) => f.write_str(text),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub timestamp: i64,
    pub content: LineContent,
}

pub fn execute_starter(
    path: &str,
    confirm: bool,
) -> (JoinHandle<Starter>, broadcast::Receiver<Vec<Line>>) {
    let path = path.to_string();

    let (tx, rx) = broadcast::channel(16);
    (
        task::spawn(async move {
            let combo = match process::Command::new(path.clone())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Err(err) => Err(InvalidSnafu {
                    reason: format!("excuting error: {err}"),
                }
                .build()),
                Ok(mut cmd) => {
                    let mut stdin = cmd.stdin.take().unwrap();
                    let stdout = cmd.stdout.take().unwrap();
                    let stderr = cmd.stderr.take().unwrap();
                    let mut stdout_reader = BufReader::new(stdout).lines();
                    let mut stderr_reader = BufReader::new(stderr).lines();

                    let mut buffer = String::new();

                    if confirm {
                        stdin
                            .write("\n".as_bytes())
                            .await
                            .inspect_err(|err| warn!(?err, "Send confirm error"))
                            .ok();
                    }
                    drop(stdin);

                    let mut stderr_closed = false;
                    let mut stdout_closed = false;
                    let mut batch = vec![];
                    let delay = tokio::time::Duration::from_millis(500);
                    loop {
                        tokio::select! {
                            line = stdout_reader.next_line(), if !stdout_closed => {
                                match line {
                                    Ok(Some(line)) => {
                                        buffer.push_str(&line);
                                        buffer.push('\n');
                                        let line = Line {
                                            timestamp: chrono::Local::now().timestamp(),
                                            content: LineContent::Stdout(line),
                                        };
                                        batch.push(line);
                                    },
                                    Ok(None) => {
                                        debug!("stdout closed");
                                        stdout_closed = true;
                                    },
                                    Err(e) => {
                                        error!(err = ?e, "Error reading stdout");
                                        stdout_closed = true;
                                    }
                                }
                            },
                            line = stderr_reader.next_line(), if !stderr_closed => {
                                match line {
                                    Ok(Some(line)) => {
                                        buffer.push_str(&line);
                                        buffer.push('\n');
                                        let line = Line {
                                            timestamp: chrono::Local::now().timestamp(),
                                            content: LineContent::Stderr(line),
                                        };
                                        batch.push(line);
                                    },
                                    Ok(None) => {
                                        debug!("stderr closed");
                                        stderr_closed = true;
                                    },
                                    Err(e) => {
                                        error!(err = ?e, "Error reading stderr");
                                        stderr_closed = true;
                                    }
                                }
                            },
                            _ = tokio::time::sleep(delay), if !stdout_closed || !stderr_closed => {
                               tx.send(batch.clone()).ok(); // It's ok that no receiver
                               batch.clear();
                            },
                            else => break, // Both streams exhausted
                        }
                    }
                    if !batch.is_empty() {
                        tx.send(batch).ok(); // It's ok that no receiver
                    }
                    debug!(buffer = buffer.as_str(), "combo output");
                    Ok(parse(&buffer))
                }
            };

            Starter { path, combo }
        }),
        rx,
    )
}

#[cfg(test)]
mod test {
    use std::os::unix::fs::PermissionsExt;

    use indoc::indoc;
    use tempfile::TempDir;

    use crate::{ComboMode, Instruction};

    use super::*;

    async fn create_temp_combo(
        name: &str,
        code: &str,
    ) -> Result<(TempDir, String), Box<dyn std::error::Error>> {
        // Create a temporary directory and test file
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join(name);

        debug!(?file_path, ?code, "create temp file");
        std::fs::write(&file_path, code)?;

        // Get current permissions
        let metadata = fs::metadata(&file_path).await?;
        let mut permissions = metadata.permissions();

        // Set the executable bit (octal 0o755 for owner read/write/execute, group read/execute, others read/execute)
        // For Windows, this might not have the same effect as on Unix-like systems,
        // as Windows handles executable status differently.
        permissions.set_mode(0o755);

        // Apply the new permissions
        fs::set_permissions(&file_path, permissions).await?;

        Ok((temp_dir, file_path.to_string_lossy().to_string()))
    }

    #[tokio::test]
    async fn execute_starter_abort() -> Result<(), Box<dyn std::error::Error>> {
        let (_guard, file_path) = create_temp_combo(
            "commit.sh",
            indoc! {r#"
            #!/usr/bin/env bash

            cat <<EOF
            ---
            name: commit
            description: Git Commit with Proper Message
            mode: bash_xtrace
            command_prefix: "$ "
            ---
            Check the recent commits and adhere to the established commit message format.

            Summarize the staged changes and commit them with a clear, concise, and formatted message as a single commit.
            EOF

            # Enter to continue, Ctrl-D to abort
            read -rs || exit
            "#},
        )
            .await?;

        let (execution, _) = execute_starter(&file_path, false);
        let Starter { path, combo } = execution.await?;
        debug!(?path, ?combo, "execute_starter success");
        assert_eq!(path, file_path);
        assert!(combo.is_ok());
        let combo = combo.unwrap();
        assert_eq!(combo.metadata.name, "commit");
        assert_eq!(combo.metadata.description, "Git Commit with Proper Message");
        assert_eq!(
            combo.metadata.mode,
            ComboMode::BashXtrace {
                command_prefix: "$ ".to_string()
            }
        );
        assert_eq!(combo.instructions.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn execute_starter_continue() -> Result<(), Box<dyn std::error::Error>> {
        let (_guard, file_path) = create_temp_combo(
            "test.sh",
            indoc! {r#"
            #!/usr/bin/env bash

            cat <<-EOF
            ---
            name: test
            mode: bash_xtrace
            command_prefix: "$ "
            ---
            EOF

            # Enter to continue, Ctrl-D to abort
            read -rs || exit

            echo "Hello world"
            "#},
        )
        .await?;

        let (execution, _) = execute_starter(&file_path, true);
        let Starter { path, combo } = execution.await?;
        debug!(?path, ?combo, "execute_starter success");
        assert_eq!(path, file_path);
        assert!(combo.is_ok());
        let combo = combo.unwrap();
        assert_eq!(combo.metadata.name, "test");
        assert_eq!(combo.metadata.description, "");
        assert_eq!(
            combo.metadata.mode,
            ComboMode::BashXtrace {
                command_prefix: "$ ".to_string()
            }
        );
        assert_eq!(combo.instructions.len(), 1);
        assert_eq!(
            combo.instructions.first(),
            Some(&Instruction::Text("Hello world".to_string()))
        );

        Ok(())
    }
}
