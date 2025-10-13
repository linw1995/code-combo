use std::process::Stdio;

use snafu::prelude::*;
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process,
};
use tracing::{debug, warn};

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
                starters.push(execute_starter(&path.to_string_lossy(), false).await);
            }
            starters
        }
        Err(err) => {
            warn!(?path, ?err, "read dir error");
            Vec::new()
        }
    }
}

pub async fn execute_starter(path: &str, confirm: bool) -> Starter {
    let path = path.to_string();

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
            loop {
                tokio::select! {
                    line = stdout_reader.next_line() => {
                        match line {
                            Ok(Some(line)) => {buffer.push_str(&line); buffer.push('\n');},
                            Ok(None) => break, // stdout closed
                            Err(e) => eprintln!("Error reading stdout: {}", e),
                        }
                    },
                    line = stderr_reader.next_line() => {
                        match line {
                            Ok(Some(line)) => {buffer.push_str(&line); buffer.push('\n');},
                            Ok(None) => break, // stderr closed
                            Err(e) => eprintln!("Error reading stderr: {}", e),
                        }
                    },
                    else => break, // Both streams exhausted
                }
            }
            debug!(buffer = buffer.as_str(), "combo output");
            Ok(parse(&buffer))
        }
    };

    Starter { path, combo }
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
            r#"
#!/bin/bash

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
            "#,
        ).await?;

        let starter = execute_starter(&file_path, false).await;
        let Starter { path, combo } = starter;
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
            #!/bin/bash

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

        let starter = execute_starter(&file_path, true).await;
        let Starter { path, combo } = starter;
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
