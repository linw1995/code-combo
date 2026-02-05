use std::time::Duration;

use tempfile::TempDir;
use vt100::Parser;

use super::support::{
    KillOnDrop, shutdown_tui, spawn_reader, spawn_tui, wait_for_screen_contains, write_config,
};

#[test]
fn tui_starts_and_exits_on_ctrl_c() {
    let temp = TempDir::new().expect("tempdir");
    let config_dir = write_config(&temp);
    let (mut child, reader, mut writer) = spawn_tui(Some(&config_dir), &[]);
    let mut guard = KillOnDrop::new(child.clone_killer());
    let rx = spawn_reader(reader);
    let mut parser = Parser::new(24, 120, 0);

    wait_for_screen_contains(&mut parser, &rx, "Ready", Duration::from_secs(20));

    let status = shutdown_tui(&mut *child, &mut writer, &parser);
    guard.disarm();
    assert!(status.success(), "tui exit failed: {status:?}");
}
