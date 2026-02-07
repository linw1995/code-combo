use std::time::Duration;

use tempfile::TempDir;
use vt100::Parser;

use super::support::{
    KillOnDrop, coco_binary, create_minimal_e2e_config, shutdown_tui, spawn_reader, spawn_tui,
    spawn_tui_with_env, wait_for_screen_contains,
};

#[test]
fn tui_starts_and_exits_on_ctrl_c() {
    let temp = create_minimal_e2e_config(true);
    let config_dir = temp.path().join("coco");
    let (mut child, reader, mut writer) = spawn_tui(Some(&config_dir), &[]);
    let mut guard = KillOnDrop::new(child.clone_killer());
    let rx = spawn_reader(reader);
    let mut parser = Parser::new(24, 120, 0);

    wait_for_screen_contains(&mut parser, &rx, "Ready", Duration::from_secs(20));

    let status = shutdown_tui(&mut *child, &mut writer, &parser);
    guard.disarm();
    assert!(status.success(), "tui exit failed: {status:?}");
}

#[test]
fn combo_run_not_found_is_visible_in_tui() {
    let temp_config = create_minimal_e2e_config(true);
    let config_dir = temp_config.path().join("coco");
    let temp = TempDir::new().expect("tempdir");
    let socket_path = temp.path().join("coco-e2e.sock");
    let socket = socket_path.to_string_lossy().to_string();
    let (mut child, reader, mut writer) = spawn_tui_with_env(
        Some(&config_dir),
        &[],
        &[("COCO_SESSION_SOCK", socket.as_str())],
    );
    let mut guard = KillOnDrop::new(child.clone_killer());
    let rx = spawn_reader(reader);
    let mut parser = Parser::new(24, 120, 0);

    wait_for_screen_contains(&mut parser, &rx, "Ready", Duration::from_secs(20));

    let status = std::process::Command::new(coco_binary())
        .args(["combo", "run", "__missing_combo__"])
        .env("COCO_SESSION_SOCK", &socket)
        .status()
        .expect("run combo client command");
    assert!(!status.success(), "missing combo should return non-zero");

    wait_for_screen_contains(&mut parser, &rx, "Not found", Duration::from_secs(20));

    let status = shutdown_tui(&mut *child, &mut writer, &parser);
    guard.disarm();
    assert!(status.success(), "tui exit failed: {status:?}");
}
