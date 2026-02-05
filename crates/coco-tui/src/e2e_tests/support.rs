use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, NativePtySystem, PtySize, PtySystem,
};
use tempfile::TempDir;
use vt100::Parser;

pub(crate) type PtyChild = dyn Child + Send;

pub(crate) fn write_config(dir: &TempDir) -> PathBuf {
    let config_dir = dir.path().join("coco");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let config_path = config_dir.join("config.toml");
    std::fs::write(&config_path, "providers = []\n").expect("write config");
    config_dir
}

#[allow(dead_code)]
pub(crate) fn require_e2e_config_dir() -> PathBuf {
    let config_dir =
        PathBuf::from(std::env::var("COCO_E2E_CONFIG_DIR").expect("set COCO_E2E_CONFIG_DIR"));
    assert!(
        config_dir.join("config.toml").exists(),
        "missing config.toml under COCO_E2E_CONFIG_DIR"
    );
    config_dir
}

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .expect("resolve repo root")
}

pub(crate) fn spawn_tui(
    config_dir: Option<&Path>,
    args: &[&str],
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

    let mut cmd = CommandBuilder::new("cargo");
    cmd.args(["run", "-p", "coco-tui", "--bin", "coco", "--"]);
    cmd.cwd(repo_root());

    if let Some(dir) = config_dir {
        cmd.args(["--config-dir", dir.to_string_lossy().as_ref()]);
    }

    cmd.env("COCO_LOG", "trace");

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
