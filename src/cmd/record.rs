use std::process::Stdio;

use futures_util::StreamExt;
use snafu::prelude::*;
use time::OffsetDateTime;
use tokio::time::Duration;
use tracing::info;

use crate::{
    RecordChunkPayload, RecordEndPayload, RecordStartPayload, SessionClientError,
    SessionSocketClient,
    error::Result,
    exec::{ChunkConfig, ExecCommand, ProcessEvent, StreamKind},
};

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
struct WrappedResult {
    cmd: Vec<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    started_at: i64,
    ended_at: i64,
}

pub async fn handle_record(wrap_result: bool, command: Vec<String>) -> Result<()> {
    ensure_whatever!(!command.is_empty(), "command is required");
    let exec_command = normalize_command(&command);

    let client = SessionSocketClient::require_from_env().await?;
    let started_at = OffsetDateTime::now_utc().unix_timestamp();
    let mut session = Some(
        client
            .begin_record(RecordStartPayload {
                command: exec_command.clone(),
                started_at,
            })
            .await
            .whatever_context("failed to begin record session")?,
    );

    let mut interrupt_task = session.as_ref().map(|sess| {
        let session_clone = sess.clone();
        tokio::spawn(async move { session_clone.listen_for_interrupt().await })
    });

    if let Some(sess) = session.as_mut() {
        sess.wait_for_allow()
            .await
            .whatever_context("record interrupted by server")?;
    }

    let started_at = OffsetDateTime::now_utc().unix_timestamp();
    let mut proc = ExecCommand::from_argv(exec_command.clone())
        .stdin(Stdio::inherit())
        .spawn_chunked(ChunkConfig {
            interval: Duration::from_millis(500),
        })
        .whatever_context("failed to spawn recorded command")?;

    let mut captured_stdout = String::new();
    let mut captured_stderr = String::new();
    let mut exit_code: Option<i32> = None;
    let mut ended_at: Option<i64> = None;

    loop {
        tokio::select! {
            ctrl = async {
                match interrupt_task.as_mut() {
                    Some(task) => Some(task.await),
                    None => None,
                }
            }, if interrupt_task.is_some() => {
                if let Some(result) = ctrl {
                    match result {
                        Ok(Err(SessionClientError::Interrupted)) => {
                            proc.killer.kill();
                        }
                        Ok(Err(err)) => {
                            eprintln!("control task error: {err}");
                        }
                        Err(err) => {
                            eprintln!("control task join error: {err}");
                        }
                        _ => ()
                    }
                }
                interrupt_task = None;
            }
            event = proc.events.next() => {
                let Some(event) = event else { break };
                match event {
                    ProcessEvent::Started { .. } => {}
                    ProcessEvent::Chunk(chunk) => {
                        on_chunk(
                            &mut session,
                            chunk.stream,
                            chunk.lines,
                            &mut captured_stdout,
                            &mut captured_stderr,
                            wrap_result,
                        ).await?;
                    }
                    ProcessEvent::Exited { timestamp, exit_code: code } => {
                        ended_at = Some(timestamp);
                        exit_code = code;
                    }
                }
            }
        }
    }

    let status = proc
        .wait()
        .await
        .whatever_context("failed to wait for recorded command")?;
    let ended_at = ended_at.unwrap_or_else(|| OffsetDateTime::now_utc().unix_timestamp());

    if let Some(sess) = session.as_ref() {
        let stdout_payload = wrap_result.then_some(captured_stdout.clone());
        let stderr_payload = wrap_result.then_some(captured_stderr.clone());
        sess.finish(RecordEndPayload {
            exit_code: exit_code.or_else(|| status.code()),
            stdout: stdout_payload,
            stderr: stderr_payload,
            ended_at,
        })
        .await
        .whatever_context("failed to send record end")?;
    }

    if wrap_result {
        let wrapped = WrappedResult {
            cmd: exec_command.clone(),
            exit_code: exit_code.or_else(|| status.code()),
            stdout: captured_stdout,
            stderr: captured_stderr,
            started_at,
            ended_at,
        };
        let json =
            serde_json::to_string(&wrapped).whatever_context("failed to serialize wrap result")?;
        println!("{json}");
    }

    let exit_code = exit_code.or_else(|| status.code()).unwrap_or(1);

    info!(exit_code, "recorded command finished");

    std::process::exit(exit_code);
}

async fn on_chunk(
    session: &mut Option<crate::RecordSession>,
    stream: StreamKind,
    lines: Vec<String>,
    captured_stdout: &mut String,
    captured_stderr: &mut String,
    wrap_result: bool,
) -> Result<()> {
    for line in &lines {
        if wrap_result {
            match stream {
                StreamKind::Stdout => {
                    captured_stdout.push_str(line);
                    captured_stdout.push('\n');
                }
                StreamKind::Stderr => {
                    captured_stderr.push_str(line);
                    captured_stderr.push('\n');
                }
            }
        } else {
            match stream {
                StreamKind::Stdout => println!("{line}"),
                StreamKind::Stderr => eprintln!("{line}"),
            }
        }
    }

    if let Some(sess) = session.as_ref() {
        sess.send_chunk(RecordChunkPayload { stream, lines })
            .await
            .whatever_context("failed to send record chunk")?;
    }
    Ok(())
}

fn normalize_command(command: &[String]) -> Vec<String> {
    if command.len() == 1 {
        return vec!["bash".into(), "-c".into(), command[0].clone()];
    }
    command.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_result_serialization() {
        let payload = WrappedResult {
            cmd: vec!["echo".into(), "hi".into()],
            exit_code: Some(0),
            stdout: "hi\n".into(),
            stderr: String::new(),
            started_at: 1,
            ended_at: 2,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"cmd\":[\"echo\",\"hi\"]"));
        assert!(json.contains("\"exit_code\":0"));
        assert!(json.contains("\"stdout\":\"hi\\n\""));
    }

    #[test]
    fn normalize_command_wraps_single_arg_with_bash() {
        let cmd = vec!["echo hi".to_string()];
        let normalized = normalize_command(&cmd);
        assert_eq!(
            normalized,
            vec!["bash".to_string(), "-c".to_string(), "echo hi".to_string()]
        );
    }

    #[test]
    fn normalize_command_preserves_args() {
        let cmd = vec!["echo".to_string(), "hi".to_string()];
        let normalized = normalize_command(&cmd);
        assert_eq!(normalized, cmd);
    }
}
