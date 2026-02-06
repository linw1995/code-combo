use std::time::Duration;

use vt100::Parser;

use super::support::{
    KillOnDrop, MockOpenAiScenario, MockOpenAiServer, assert_screen_not_contains,
    create_mock_e2e_config, send_alt_enter, send_enter, send_escape, send_text, shutdown_tui,
    spawn_reader, spawn_tui, wait_for_screen_contains,
};

const COMBO_NAME: &str = "e2e_mock_interactive";
const COMBO_NAME_NON_INTERACTIVE: &str = "e2e_mock_non_interactive";
type ComboHarness = (
    KillOnDrop,
    Box<dyn std::io::Write + Send>,
    std::sync::mpsc::Receiver<Vec<u8>>,
    Parser,
    Box<super::support::PtyChild>,
    tempfile::TempDir,
);

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

fn mock_combo_script_non_interactive() -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

input="${{*:-Long time no see}}"

coco metadata name={COMBO_NAME_NON_INTERACTIVE} description="mock non-interactive combo" || exit 0

response="$(coco ask --schemas result:result --schemas reason:reason "Process input: ${{input}}")"
echo "$response"
"#
    )
}

fn run_combo_with_mock(mock: &MockOpenAiServer) -> ComboHarness {
    run_combo_with_mock_and_auto_accept(mock, false)
}

fn run_combo_with_mock_and_auto_accept(
    mock: &MockOpenAiServer,
    auto_accept_edits: bool,
) -> ComboHarness {
    run_combo_with_script(mock, auto_accept_edits, COMBO_NAME, &mock_combo_script())
}

fn run_combo_non_interactive_with_mock(mock: &MockOpenAiServer) -> ComboHarness {
    run_combo_with_script(
        mock,
        false,
        COMBO_NAME_NON_INTERACTIVE,
        &mock_combo_script_non_interactive(),
    )
}

fn run_combo_with_script(
    mock: &MockOpenAiServer,
    auto_accept_edits: bool,
    combo_name: &str,
    combo_script: &str,
) -> ComboHarness {
    let temp = create_mock_e2e_config(mock.base_url(), auto_accept_edits, combo_name, combo_script);
    let config_dir = temp.path().join("coco");
    let (child, reader, writer) = spawn_tui(
        Some(&config_dir),
        &["combo", "run", combo_name, "Long time no see"],
    );
    let guard = KillOnDrop::new(child.clone_killer());
    let rx = spawn_reader(reader);
    let parser = Parser::new(24, 120, 0);
    (guard, writer, rx, parser, child, temp)
}

fn assert_shutdown_ok(
    child: &mut super::support::PtyChild,
    writer: &mut dyn std::io::Write,
    parser: &Parser,
    guard: &mut KillOnDrop,
) {
    let status = shutdown_tui(child, writer, parser);
    guard.disarm();
    assert!(status.success(), "tui exit failed: {status:?}");
}

#[test]
fn combo_interactive_shows_feedback_prompt_before_reply_tool_use() {
    let mock = MockOpenAiServer::start("E2E_TOKEN_PROMPT");
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

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

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
    assert!(
        mock.request_count() >= 1,
        "expected at least one LLM call, got {}",
        mock.request_count()
    );
    assert!(
        !mock.saw_feedback_token(),
        "unexpectedly observed feedback token before user input"
    );
}

#[test]
fn combo_interactive_allows_feedback_before_coco_reply_tool_use() {
    let mock = MockOpenAiServer::start("E2E_FEEDBACK_TOKEN");
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Please provide feedback first",
        Duration::from_secs(20),
    );
    send_text(&mut *writer, "Use user feedback E2E_FEEDBACK_TOKEN");
    send_alt_enter(&mut *writer);
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

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
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

#[test]
fn combo_interactive_requires_two_feedback_rounds_before_reply_tool_use() {
    let mock =
        MockOpenAiServer::start_with_scenario(MockOpenAiScenario::RequireFeedbackTokens(vec![
            "E2E_TOKEN_ONE".to_string(),
            "E2E_TOKEN_TWO".to_string(),
        ]));
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Please provide feedback first",
        Duration::from_secs(20),
    );

    send_text(&mut *writer, "first round E2E_TOKEN_ONE");
    send_alt_enter(&mut *writer);
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "missing: E2E_TOKEN_TWO",
        Duration::from_secs(30),
    );
    assert_screen_not_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(2),
    );

    send_text(&mut *writer, "second round E2E_TOKEN_TWO");
    send_alt_enter(&mut *writer);
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
    assert!(mock.saw_token("E2E_TOKEN_ONE"));
    assert!(mock.saw_token("E2E_TOKEN_TWO"));
}

#[test]
fn combo_interactive_reply_tool_use_command_contains_required_fields() {
    let mock = MockOpenAiServer::start("E2E_CMD_TOKEN");
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Please provide feedback first",
        Duration::from_secs(20),
    );
    send_text(&mut *writer, "feedback E2E_CMD_TOKEN");
    send_alt_enter(&mut *writer);
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "coco reply --result='mock polished result'",
        Duration::from_secs(30),
    );
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "--reason='used user feedback'",
        Duration::from_secs(30),
    );
    assert_screen_not_contains(
        &mut parser,
        &rx,
        "COCO_SESSION_SOCK=",
        Duration::from_secs(2),
    );

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
}

#[test]
fn combo_interactive_rate_limit_error_is_visible() {
    let mock = MockOpenAiServer::start_with_scenario(MockOpenAiScenario::RateLimited);
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(&mut parser, &rx, "chat failed:", Duration::from_secs(30));
    wait_for_screen_contains(&mut parser, &rx, "429", Duration::from_secs(30));
    assert_screen_not_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(2),
    );

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
    assert!(mock.request_count() >= 1);
}

#[test]
fn combo_interactive_rate_limit_does_not_fake_complete() {
    let mock = MockOpenAiServer::start_with_scenario(MockOpenAiScenario::RateLimited);
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(&mut parser, &rx, "chat failed:", Duration::from_secs(30));
    assert_screen_not_contains(&mut parser, &rx, "Completed", Duration::from_secs(2));

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
}

#[test]
fn combo_interactive_auto_accept_edits_still_requires_bash_confirmation() {
    let mock = MockOpenAiServer::start_with_scenario(MockOpenAiScenario::ImmediateReply);
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) =
        run_combo_with_mock_and_auto_accept(&mock, true);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Combo: e2e_mock_interactive",
        Duration::from_secs(30),
    );
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "coco reply --result='mock polished result'",
        Duration::from_secs(30),
    );
    assert_screen_not_contains(&mut parser, &rx, "Completed", Duration::from_secs(2));

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
}

#[test]
fn combo_interactive_approval_executes_coco_reply_and_completes() {
    let mock = MockOpenAiServer::start_with_scenario(MockOpenAiScenario::ImmediateReply);
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );
    send_enter(&mut *writer);
    wait_for_screen_contains(&mut parser, &rx, "Completed", Duration::from_secs(30));
    assert_screen_not_contains(
        &mut parser,
        &rx,
        "failed to read COCO_SESSION_SOCK",
        Duration::from_secs(2),
    );
    child
        .clone_killer()
        .kill()
        .expect("kill tui after assertion");
    let _ = child.try_wait();
    guard.disarm();
}

#[test]
fn combo_interactive_coco_reply_reject_then_feedback_can_complete() {
    const FEEDBACK_TOKEN: &str = "E2E_REJECT_FEEDBACK";
    let mock = MockOpenAiServer::start_with_scenario(
        MockOpenAiScenario::ImmediateReplyThenRequireFeedbackToken(FEEDBACK_TOKEN.to_string()),
    );
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );
    send_escape(&mut *writer);
    wait_for_screen_contains(&mut parser, &rx, "Cancelled", Duration::from_secs(30));

    // Ensure focus returns to input before submitting feedback.
    send_escape(&mut *writer);
    send_text(&mut *writer, "i");
    send_text(&mut *writer, "retry with feedback E2E_REJECT_FEEDBACK");
    send_alt_enter(&mut *writer);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(30),
    );
    send_enter(&mut *writer);
    wait_for_screen_contains(&mut parser, &rx, "Completed", Duration::from_secs(30));

    assert!(
        mock.saw_token(FEEDBACK_TOKEN),
        "expected feedback token in model request after cancellation"
    );

    child
        .clone_killer()
        .kill()
        .expect("kill tui after assertion");
    let _ = child.try_wait();
    guard.disarm();
}

#[test]
fn combo_interactive_reject_then_feedback_allows_second_reply_attempt() {
    let mock = MockOpenAiServer::start_with_scenario(
        MockOpenAiScenario::RejectThenNeedFeedbackToken("E2E_RETRY_REASON".to_string()),
    );
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) = run_combo_with_mock(&mock);

    wait_for_screen_contains(&mut parser, &rx, "rm -rf /", Duration::from_secs(30));
    wait_for_screen_contains(&mut parser, &rx, "Ready", Duration::from_secs(30));
    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Please provide feedback first (missing: E2E_RETRY_REASON).",
        Duration::from_secs(30),
    );
    assert_screen_not_contains(
        &mut parser,
        &rx,
        "Bash Awaiting confirmation",
        Duration::from_secs(2),
    );
    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
    assert!(mock.request_count() >= 2);
}

#[test]
fn combo_non_interactive_executes_reply_without_confirmation() {
    let mock = MockOpenAiServer::start_with_scenario(MockOpenAiScenario::ImmediateReply);
    let (mut guard, mut writer, rx, mut parser, mut child, _temp) =
        run_combo_non_interactive_with_mock(&mock);

    wait_for_screen_contains(
        &mut parser,
        &rx,
        "Combo: e2e_mock_non_interactive",
        Duration::from_secs(30),
    );
    // Non-interactive ask should finish without any user approval input.
    wait_for_screen_contains(&mut parser, &rx, "Completed", Duration::from_secs(10));

    assert_shutdown_ok(&mut *child, &mut *writer, &parser, &mut guard);
}
