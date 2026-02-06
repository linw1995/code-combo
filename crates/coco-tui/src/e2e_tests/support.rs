use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use code_combo::{EnvString, load_config_with_overrides, workspace_config_path};
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, NativePtySystem, PtySize, PtySystem,
};
use tempfile::TempDir;
use vt100::Parser;

pub(crate) type PtyChild = dyn Child + Send;

pub(crate) fn require_e2e_config_dir() -> PathBuf {
    let config_dir =
        PathBuf::from(std::env::var("COCO_E2E_CONFIG_DIR").expect("set COCO_E2E_CONFIG_DIR"));
    assert!(
        config_dir.join("config.toml").exists(),
        "missing config.toml under COCO_E2E_CONFIG_DIR"
    );
    config_dir
}

#[allow(dead_code)]
pub(crate) fn missing_provider_envs(config_dir: &Path) -> Vec<String> {
    let config_path = config_dir.join("config.toml");
    let workspace_path = workspace_config_path();
    let config = load_config_with_overrides(&config_path, config_dir, Some(&workspace_path))
        .expect("load e2e config");

    let mut missing = Vec::new();
    for provider in config.providers {
        if let EnvString::EnvVar { name, .. } = provider.api_key
            && env::var(&name).is_err()
            && !missing.contains(&name)
        {
            missing.push(name);
        }
    }
    missing
}

#[allow(dead_code)]
pub(crate) fn create_e2e_config_with_auto_accept(auto_accept_edits: bool) -> TempDir {
    let source_config_dir = require_e2e_config_dir();
    let temp = TempDir::new().expect("create e2e temp config dir");
    let config_dir = temp.path().join("coco");
    fs::create_dir_all(&config_dir).expect("create temp config dir");

    let source_config_path = source_config_dir.join("config.toml");
    let target_config_path = config_dir.join("config.toml");
    fs::copy(&source_config_path, &target_config_path).expect("copy base config");

    let override_path = config_dir.join("config.overwrite.toml");
    fs::write(
        &override_path,
        format!("[agent]\nauto_accept_edits = {auto_accept_edits}\n"),
    )
    .expect("write runtime overrides");

    temp
}

pub(crate) fn create_mock_e2e_config(
    base_url: &str,
    auto_accept_edits: bool,
    combo_name: &str,
    combo_script: &str,
) -> TempDir {
    let temp = TempDir::new().expect("create e2e temp config dir");
    let config_dir = temp.path().join("coco");
    let combos_dir = config_dir.join("combos");
    fs::create_dir_all(&combos_dir).expect("create temp combo dir");

    let config_path = config_dir.join("config.toml");
    let config_content = format!(
        r#"[ui.notifications]
enabled = false

[agent]
auto_accept_edits = {auto_accept_edits}

[[providers]]
name = "mock-openai"
kind = "openai"
api_key = "mock-api-key"
base_url = "{base_url}"
disable_stream = true
offload_combo_reply = true
"#
    );
    fs::write(&config_path, config_content).expect("write mock e2e config");

    let combo_path = combos_dir.join(format!("{combo_name}.sh"));
    fs::write(&combo_path, combo_script).expect("write combo script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&combo_path)
            .expect("combo metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&combo_path, perms).expect("set combo executable permissions");
    }

    temp
}

pub(crate) fn create_minimal_e2e_config(auto_accept_edits: bool) -> TempDir {
    let temp = TempDir::new().expect("create e2e temp config dir");
    let config_dir = temp.path().join("coco");
    fs::create_dir_all(config_dir.join("combos")).expect("create temp combo dir");

    let config_path = config_dir.join("config.toml");
    let config_content = format!(
        r#"[ui.notifications]
enabled = false

[agent]
auto_accept_edits = {auto_accept_edits}

[[providers]]
name = "minimal-openai"
kind = "openai"
api_key = "test-api-key"
base_url = "http://127.0.0.1:1"
disable_stream = true
"#
    );
    fs::write(&config_path, config_content).expect("write minimal e2e config");
    temp
}

#[derive(Debug, Default)]
struct MockOpenAiState {
    request_count: usize,
    saw_feedback_token: bool,
}

pub(crate) struct MockOpenAiServer {
    base_url: String,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<MockOpenAiState>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockOpenAiServer {
    pub(crate) fn start(feedback_token: impl Into<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock openai listener");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let addr = listener.local_addr().expect("get listener addr");
        let base_url = format!("http://{addr}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(MockOpenAiState::default()));
        let feedback_token = feedback_token.into();
        let worker = thread::spawn({
            let shutdown = shutdown.clone();
            let state = state.clone();
            let feedback_token = feedback_token.clone();
            move || {
                while !shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            handle_mock_openai_connection(&mut stream, &feedback_token, &state);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        Self {
            base_url,
            shutdown,
            state,
            worker: Some(worker),
        }
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn request_count(&self) -> usize {
        self.state.lock().expect("lock mock state").request_count
    }

    pub(crate) fn saw_feedback_token(&self) -> bool {
        self.state
            .lock()
            .expect("lock mock state")
            .saw_feedback_token
    }
}

impl Drop for MockOpenAiServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Unblock accept quickly.
        let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_mock_openai_connection(
    stream: &mut TcpStream,
    feedback_token: &str,
    state: &Arc<Mutex<MockOpenAiState>>,
) {
    let Ok((path, body)) = read_http_request(stream) else {
        let _ = write_http_response(stream, 400, br#"{"error":"bad request"}"#);
        return;
    };
    if path != "/v1/chat/completions" && path != "/chat/completions" {
        let _ = write_http_response(stream, 404, br#"{"error":"not found"}"#);
        return;
    }

    let request: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            let _ = write_http_response(stream, 400, br#"{"error":"invalid json"}"#);
            return;
        }
    };
    let response = build_mock_openai_response(&request, feedback_token, state);
    let body = serde_json::to_vec(&response).expect("serialize mock openai response");
    let _ = write_http_response(stream, 200, &body);
}

fn read_http_request(stream: &mut TcpStream) -> Result<(String, Vec<u8>), std::io::Error> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "missing request line",
        ));
    }
    let mut parts = request_line.split_whitespace();
    let _method = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing method"))?;
    let path = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing path"))?
        .to_string();

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("content-length:") {
            content_length = value.trim().parse::<usize>().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok((path, body))
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
) -> Result<(), std::io::Error> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn build_mock_openai_response(
    request: &serde_json::Value,
    feedback_token: &str,
    state: &Arc<Mutex<MockOpenAiState>>,
) -> serde_json::Value {
    let user_text = request
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| {
                    let role = message.get("role").and_then(|value| value.as_str())?;
                    if role != "user" {
                        return None;
                    }
                    message
                        .get("content")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let is_summary_prompt = user_text.contains("Summarize the combo execution for the user.");
    let saw_feedback = user_text.contains(feedback_token);

    {
        let mut guard = state.lock().expect("lock mock state");
        guard.request_count += 1;
        guard.saw_feedback_token |= saw_feedback;
    }

    if is_summary_prompt {
        return mock_text_response(
            "- status: success\n- interaction: feedback captured\n- output: reply fields generated",
        );
    }
    if saw_feedback {
        let command = "coco reply --result='mock polished result' --reason='used user feedback'";
        return mock_bash_tool_call_response(command);
    }

    // Slow down no-feedback turns so the UI has time to capture manual input.
    thread::sleep(Duration::from_millis(120));
    mock_text_response(&format!(
        "Please provide feedback first (include token: {feedback_token})."
    ))
}

fn mock_text_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 10,
            "total_tokens": 20
        }
    })
}

fn mock_bash_tool_call_response(command: &str) -> serde_json::Value {
    let arguments = serde_json::json!({ "command": command }).to_string();
    serde_json::json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_mock_bash",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 14,
            "total_tokens": 26
        }
    })
}

pub(crate) fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .expect("resolve repo root")
}

fn resolve_coco_bin_from_env() -> Option<PathBuf> {
    if let Ok(path) = env::var("COCO_TEST_BIN") {
        return Some(PathBuf::from(path));
    }
    if let Ok(path) = env::var("COCO_TUI_BIN") {
        return Some(PathBuf::from(path));
    }
    if let Ok(path) = env::var("CARGO_BIN_EXE_coco") {
        return Some(PathBuf::from(path));
    }
    None
}

fn resolve_coco_bin_from_target() -> PathBuf {
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let base = if let Some(target_dir) = env::var_os("CARGO_TARGET_DIR") {
        PathBuf::from(target_dir).join(profile)
    } else {
        repo_root().join("target").join(profile)
    };
    let mut path = base.join("coco");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

pub(crate) fn coco_binary() -> PathBuf {
    let path = resolve_coco_bin_from_env().unwrap_or_else(resolve_coco_bin_from_target);
    assert!(
        path.exists(),
        "coco binary not found at {:?}; build `cargo build -p coco-tui --bin coco` or set COCO_TUI_BIN/COCO_TEST_BIN",
        path
    );
    path
}

pub(crate) fn spawn_tui(
    config_dir: Option<&Path>,
    args: &[&str],
) -> (Box<PtyChild>, Box<dyn Read + Send>, Box<dyn Write + Send>) {
    spawn_tui_with_env(config_dir, args, &[])
}

pub(crate) fn spawn_tui_with_env(
    config_dir: Option<&Path>,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (Box<PtyChild>, Box<dyn Read + Send>, Box<dyn Write + Send>) {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(coco_binary());
    cmd.cwd(repo_root());

    if let Some(dir) = config_dir {
        cmd.args(["--config-dir", dir.to_string_lossy().as_ref()]);
    }

    cmd.env("COCO_LOG", "trace");
    for (key, value) in envs {
        cmd.env(key, value);
    }

    if !args.is_empty() {
        cmd.args(args);
    }

    let child = pair.slave.spawn_command(cmd).expect("spawn coco");
    let reader = pair.master.try_clone_reader().expect("clone reader");
    let writer = pair.master.take_writer().expect("take writer");
    (child, reader, writer)
}

pub(crate) fn wait_for_screen_contains(
    parser: &mut Parser,
    rx: &Receiver<Vec<u8>>,
    needle: &str,
    timeout: Duration,
) {
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed > timeout {
            let screen = parser.screen().contents();
            panic!("timeout waiting for screen to contain: {needle}\n--- screen ---\n{screen}");
        }

        let remaining = timeout.saturating_sub(elapsed);
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(chunk) => {
                if !chunk.is_empty() {
                    parser.process(&chunk);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                panic!("pty reader thread exited before finding: {needle}");
            }
        }

        let screen = parser.screen().contents();
        if screen.contains(needle) {
            return;
        }
    }
}

pub(crate) fn assert_screen_not_contains(
    parser: &mut Parser,
    rx: &Receiver<Vec<u8>>,
    needle: &str,
    timeout: Duration,
) {
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed > timeout {
            return;
        }
        let remaining = timeout.saturating_sub(elapsed);
        match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(chunk) => {
                if !chunk.is_empty() {
                    parser.process(&chunk);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let screen = parser.screen().contents();
        assert!(
            !screen.contains(needle),
            "unexpected content found in screen: {needle}\n--- screen ---\n{screen}"
        );
    }
}

#[allow(dead_code)]
pub(crate) fn send_enter(writer: &mut dyn Write) {
    writer.write_all(&[0x0d]).expect("write enter");
    writer.flush().expect("flush");
}

#[allow(dead_code)]
pub(crate) fn send_alt_enter(writer: &mut dyn Write) {
    writer.write_all(&[0x1b, 0x0d]).expect("write alt-enter");
    writer.flush().expect("flush");
}

#[allow(dead_code)]
pub(crate) fn send_text(writer: &mut dyn Write, text: &str) {
    writer.write_all(text.as_bytes()).expect("write text");
    writer.flush().expect("flush");
}

fn send_ctrl_c(writer: &mut dyn Write) {
    writer.write_all(&[0x03]).expect("write ctrl-c");
    writer.flush().expect("flush");
}

fn send_ctrl_q(writer: &mut dyn Write) {
    writer.write_all(&[0x11]).expect("write ctrl-q");
    writer.flush().expect("flush");
}

pub(crate) fn spawn_reader(mut reader: Box<dyn Read + Send>) -> Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Vec::new());
                    break;
                }
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(_) => {
                    let _ = tx.send(Vec::new());
                    break;
                }
            }
        }
    });
    rx
}

fn wait_for_exit(
    child: &mut PtyChild,
    parser: &Parser,
    timeout: Duration,
) -> portable_pty::ExitStatus {
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait child") {
            Some(status) => return status,
            None => {
                if start.elapsed() > timeout {
                    let screen = parser.screen().contents();
                    panic!("timeout waiting for child to exit\n--- screen ---\n{screen}");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn wait_for_exit_optional(
    child: &mut PtyChild,
    timeout: Duration,
) -> Option<portable_pty::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait child") {
            Some(status) => return Some(status),
            None => {
                if start.elapsed() > timeout {
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

pub(crate) struct KillOnDrop {
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
}

impl KillOnDrop {
    pub(crate) fn new(killer: Box<dyn ChildKiller + Send + Sync>) -> Self {
        Self {
            killer: Some(killer),
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.killer = None;
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut killer) = self.killer.take() {
            let _ = killer.kill();
        }
    }
}

pub(crate) fn shutdown_tui(
    child: &mut PtyChild,
    writer: &mut dyn Write,
    parser: &Parser,
) -> ExitStatus {
    send_ctrl_c(writer);
    thread::sleep(Duration::from_millis(200));
    send_ctrl_c(writer);

    if let Some(status) = wait_for_exit_optional(child, Duration::from_secs(2)) {
        return status;
    }

    send_ctrl_q(writer);
    thread::sleep(Duration::from_millis(200));
    send_ctrl_q(writer);
    wait_for_exit(child, parser, Duration::from_secs(5))
}
