use std::time::Duration;

use vt100::Parser;

use super::support::{
    KillOnDrop, MockOpenAiServer, assert_screen_not_contains, create_mock_e2e_config,
    send_alt_enter, send_text, shutdown_tui, spawn_reader, spawn_tui, wait_for_screen_contains,
};

const COMBO_NAME: &str = "e2e_mock_interactive";

fn mock_combo_script() -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

input="${{*:-Long time no see}}"

coco metadata name={COMBO_NAME} description="mock interactive combo" || exit 0

response="$(coco ask -i --schemas result:result --schemas reason:reason "Process input: ${{input}}")"
echo "$response"
"#
    )
}

#[test]
fn combo_interactive_allows_feedback_before_coco_reply_tool_use() {
    let mock = MockOpenAiServer::start("E2E_FEEDBACK_TOKEN");
    let temp = create_mock_e2e_config(mock.base_url(), false, COMBO_NAME, &mock_combo_script());
    let config_dir = temp.path().join("coco");
    let (mut child, reader, mut writer) = spawn_tui(
        Some(&config_dir),
        &["combo", "run", COMBO_NAME, "Long time no see"],
    );
    let mut guard = KillOnDrop::new(child.clone_killer());
    let rx = spawn_reader(reader);
    let mut parser = Parser::new(24, 120, 0);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Combo: e2e_mock_interactive",
        Duration::from_secs(30),
    );

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Please provide feedback first",
        Duration::from_secs(20),
    );

    assert_screen_not_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(2),
    );

    send_text(&mut writer, "Use user feedback E2E_FEEDBACK_TOKEN");
    send_alt_enter(&mut writer);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "E2E_FEEDBACK_TOKEN",
        Duration::from_secs(30),
    );

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );

    let status = shutdown_tui(&mut *child, &mut writer, &parser);
    guard.disarm();
    assert!(status.success(), "tui exit failed: {status:?}");

    assert!(
        mock.saw_feedback_token(),
        "mock provider never observed user feedback token in LLM request history"
    );
    assert!(
        mock.request_count() >= 2,
        "expected at least two model requests, got {}",
        mock.request_count()
    );
}
