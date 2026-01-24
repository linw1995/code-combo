use std::{
    io::{self, Write},
    process::Stdio,
};

use code_combo::NotificationBackend;
use serde::Serialize;
use tokio::{io::AsyncWriteExt, process::Command};
use tracing::{debug, warn};

const OSC_PREFIX: &str = "\x1b]9;";
const OSC_SUFFIX: &str = "\x07";
const MAX_TITLE_LEN: usize = 64;
const MAX_BODY_LEN: usize = 160;

#[derive(Serialize)]
struct NotificationPayload {
    title: String,
    body: String,
}

pub fn send_notification(title: &str, body: &str, backend: &NotificationBackend) {
    if title.trim().is_empty() && body.trim().is_empty() {
        return;
    }
    match backend {
        NotificationBackend::Osc9 => {
            debug!(
                backend = "osc9",
                title_len = title.len(),
                body_len = body.len(),
                "sending notification"
            );
            send_osc9(title, body);
        }
        NotificationBackend::ExternalCommand { executable, args } => {
            debug!(
                backend = "external_command",
                executable = %executable,
                args = ?args,
                title_len = title.len(),
                body_len = body.len(),
                "sending notification"
            );
            send_external_command(executable, args, title, body);
        }
    }
}

fn send_osc9(title: &str, body: &str) {
    let Some(seq) = build_osc9(title, body) else {
        return;
    };
    let mut stdout = io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

fn build_osc9(title: &str, body: &str) -> Option<String> {
    let title = normalize_field(title, MAX_TITLE_LEN, true);
    let body = normalize_field(body, MAX_BODY_LEN, true);
    if title.is_empty() && body.is_empty() {
        return None;
    }
    let payload = if !title.is_empty() && !body.is_empty() {
        format!("{title};{body}")
    } else if !title.is_empty() {
        title
    } else {
        body
    };
    Some(format!("{OSC_PREFIX}{payload}{OSC_SUFFIX}"))
}

fn send_external_command(executable: &str, args: &[String], title: &str, body: &str) {
    let title = normalize_field(title, MAX_TITLE_LEN, false);
    let body = normalize_field(body, MAX_BODY_LEN, false);
    if title.is_empty() && body.is_empty() {
        return;
    }
    let executable = normalize_executable_path(executable);
    let payload = NotificationPayload { title, body };
    let payload_json = match serde_json::to_vec(&payload) {
        Ok(json) => json,
        Err(err) => {
            warn!(?err, "failed to serialize notification payload");
            return;
        }
    };

    let args = args.to_vec();
    tokio::spawn(async move {
        let mut cmd = Command::new(&executable);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!(?err, executable = %executable, args = ?args, "failed to spawn notification command");
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&payload_json).await.inspect_err(|err| {
                warn!(?err, executable = %executable, "failed to write notification payload");
            });
        } else {
            warn!(executable = %executable, "notification command stdin unavailable");
        }
        match child.wait().await {
            Ok(status) => {
                if !status.success() {
                    warn!(?status, executable = %executable, "notification command failed");
                }
            }
            Err(err) => {
                warn!(?err, executable = %executable, "failed to wait for notification command");
            }
        }
    });
}

fn normalize_field(value: &str, max_len: usize, replace_semicolon: bool) -> String {
    if max_len == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(value.len().min(max_len));
    let mut prev_space = false;
    for mut ch in value.chars() {
        if ch.is_control() {
            if matches!(ch, '\n' | '\r' | '\t') {
                ch = ' ';
            } else {
                continue;
            }
        }
        if replace_semicolon && ch == ';' {
            ch = ':';
        }
        if ch == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        if out.len() + ch.len_utf8() > max_len {
            break;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

fn normalize_executable_path(executable: &str) -> String {
    if let Some(rest) = executable.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    if executable == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return home;
    }
    executable.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_builds_payload_with_title_and_body() {
        let seq = build_osc9("coco", "Reply ready").expect("sequence");
        assert!(seq.starts_with(OSC_PREFIX));
        assert!(seq.ends_with(OSC_SUFFIX));
        let payload = &seq[OSC_PREFIX.len()..seq.len() - OSC_SUFFIX.len()];
        assert_eq!(payload, "coco;Reply ready");
    }

    #[test]
    fn osc9_sanitizes_control_chars_and_semicolons() {
        let seq = build_osc9("co;co", "line1\nline2\x1b[31m").expect("sequence");
        let payload = &seq[OSC_PREFIX.len()..seq.len() - OSC_SUFFIX.len()];
        assert_eq!(payload, "co:co;line1 line2[31m");
    }

    #[test]
    fn notification_payload_json_is_stable() {
        let payload = NotificationPayload {
            title: "coco".to_string(),
            body: "Reply ready".to_string(),
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json, "{\"title\":\"coco\",\"body\":\"Reply ready\"}");
    }

    #[test]
    fn normalize_executable_expands_home() {
        let original = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", "/tmp/notify-home") };
        assert_eq!(
            normalize_executable_path("~/bin/notify"),
            "/tmp/notify-home/bin/notify"
        );
        assert_eq!(normalize_executable_path("~"), "/tmp/notify-home");
        if let Some(value) = original {
            unsafe { std::env::set_var("HOME", value) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
    }
}
