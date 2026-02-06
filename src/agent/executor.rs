use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use lazy_static::lazy_static;
use serde_json::Value;
use snafu::prelude::*;
use tokio_util::sync::CancellationToken;

use crate::{
    AppliedTextEdit, ComboEvent, OutputChunk, TextEdit, error,
    tools::{
        self, BASH_TOOL_NAME, BashInput, BashTool, Final, LIST_TOOL_NAME, ListTool, READ_TOOL_NAME,
        RUN_TASK_TOOL_NAME, ReadTool, RunTaskTool, STR_REPLACE_TOOL_NAME, StrReplaceTool,
        SubagentEvent, Tool, extra_envs_for_bash_input, run_bash_chunked, run_task,
    },
};

#[derive(Clone)]
pub struct Executor {
    tools: HashMap<String, Arc<dyn Tool>>,
    tools_pcl: HashMap<String, Vec<(String, PermissionControl)>>,
    tools_once_pcl: HashMap<String, Vec<String>>,
    bash_session_allowlist: HashSet<String>,
    bash_envs: Vec<(String, String)>,
    auto_accept_edits: bool,
}

lazy_static! {
    static ref BASH_TOOL: Arc<dyn Tool + 'static> = Arc::new(BashTool::default());
    static ref READ_TOOL: Arc<dyn Tool + 'static> = Arc::new(ReadTool::default());
    static ref LIST_TOOL: Arc<dyn Tool + 'static> = Arc::new(ListTool::default());
    static ref STR_REPLACE_TOOL: Arc<dyn Tool + 'static> = Arc::new(StrReplaceTool::default());
    static ref DEFAULT_TOOLS: HashMap<String, Arc<dyn Tool + 'static>> = {
        let mut m = HashMap::<String, Arc<dyn Tool + 'static>>::new();
        m.extend(
            [
                BASH_TOOL.clone(),
                READ_TOOL.clone(),
                LIST_TOOL.clone(),
                STR_REPLACE_TOOL.clone(),
            ]
            .into_iter()
            .map(|t| (t.name().to_string(), t)),
        );
        m
    };
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            tools_pcl: HashMap::default(),
            tools_once_pcl: HashMap::default(),
            tools: DEFAULT_TOOLS.clone(),
            bash_session_allowlist: HashSet::default(),
            bash_envs: Vec::new(),
            auto_accept_edits: false,
        }
    }
}

#[derive(Snafu, Debug)]
pub enum ExecuteError {
    #[snafu(display("tool {name:?} is not found"))]
    NotFound { name: String },
}

pub use tools::Input;
//#[derive(Debug)]
//pub enum Input<'a> {
//    ToolInput(tools::Input<'a>),
//}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteStatus {
    Completed,
    Cancelled,
}

#[derive(Debug)]
pub enum Output {
    Success(Final),
    Failure(Final),
    TextEdit(TextEdit),
    ToolOutput(OutputChunk),
    SubagentOutput(SubagentEvent),
    ComboOutput(ComboEvent),
    Denied,
    AskPermission,
}

impl From<tools::ExecuteResult> for Output {
    fn from(value: tools::ExecuteResult) -> Self {
        match value {
            Err(output) => Output::Failure(output),
            Ok(output) => match output {
                tools::Output::Final(output) => Output::Success(output),
                tools::Output::TextEdit(edit) => Output::TextEdit(edit),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub enum PermissionControl {
    Once(String),
    Session,
    Always,
}

fn bash_permission_key_from_value(value: &serde_json::Value) -> Option<String> {
    let input: BashInput = serde_json::from_value(value.clone()).ok()?;
    let trimmed = input.command.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn bash_permission_key(input: &Input<'_>) -> Option<String> {
    match input {
        Input::Starter(value) => bash_permission_key_from_value(value),
        _ => None,
    }
}

impl Executor {
    /// Register a new tool dynamically.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    pub fn set_auto_accept_edits(&mut self, enabled: bool) {
        self.auto_accept_edits = enabled;
    }

    pub fn auto_accept_edits(&self) -> bool {
        self.auto_accept_edits
    }

    /// Set environment variables to inject when executing bash commands.
    pub fn set_bash_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        // Remove existing entry with same key if present
        self.bash_envs.retain(|(k, _)| k != &key);
        self.bash_envs.push((key, value.into()));
    }

    /// Remove an environment variable from bash command injection.
    pub fn remove_bash_env(&mut self, key: &str) {
        self.bash_envs.retain(|(k, _)| k != key);
    }

    pub fn apply_tool_policies(
        &mut self,
        allow_tools: Option<&[String]>,
        deny_tools: Option<&[String]>,
    ) {
        let allow = allow_tools.map(Self::normalize_tool_names);
        let deny = deny_tools
            .map(Self::normalize_tool_names)
            .unwrap_or_default();

        let allowed = match allow {
            Some(mut allow) => {
                if !deny.is_empty() {
                    allow.retain(|name| !deny.contains(name));
                }
                Some(allow)
            }
            None => {
                if deny.is_empty() {
                    None
                } else {
                    Some(
                        self.tools
                            .keys()
                            .filter(|name| !deny.contains(name.as_str()))
                            .cloned()
                            .collect::<HashSet<String>>(),
                    )
                }
            }
        };

        let Some(allowed) = allowed else {
            return;
        };

        self.tools.retain(|name, _| allowed.contains(name.as_str()));
        self.tools_pcl
            .retain(|name, _| allowed.contains(name.as_str()));
        self.tools_once_pcl
            .retain(|name, _| allowed.contains(name.as_str()));
        if !allowed.contains(BASH_TOOL_NAME) {
            self.bash_session_allowlist.clear();
        }
    }

    pub fn take_once_permission(&mut self, name: &str, id: &str) -> bool {
        if let Some(pcl) = self.tools_once_pcl.get_mut(name) {
            let mut granted_idx: Option<usize> = None;
            for (idx, granted) in pcl.iter().enumerate() {
                if granted == id {
                    granted_idx = Some(idx)
                }
            }
            if let Some(idx) = granted_idx {
                pcl.remove(idx);
                return true;
            }
        }
        false
    }

    pub async fn execute<'a>(
        &mut self,
        id: &str,
        name: &str,
        input: Input<'a>,
    ) -> error::Result<Output> {
        let mut final_output: Option<Output> = None;
        let cancel_token = CancellationToken::new();
        let status = self
            .execute_with_output(id, name, input, cancel_token, |out| {
                if !matches!(out, Output::ToolOutput(_)) {
                    final_output = Some(out);
                }
            })
            .await?;
        if matches!(status, ExecuteStatus::Cancelled) {
            return Ok(Output::Denied);
        }
        Ok(final_output.unwrap_or(Output::Denied))
    }

    pub async fn execute_with_output<'a, F>(
        &mut self,
        id: &str,
        name: &str,
        input: Input<'a>,
        cancel_token: CancellationToken,
        mut on_output: F,
    ) -> error::Result<ExecuteStatus>
    where
        F: FnMut(Output) + Send,
    {
        if cancel_token.is_cancelled() {
            return Ok(ExecuteStatus::Cancelled);
        }

        // Check tool
        let Some(tool) = self.tools.get(name).cloned() else {
            return Err(NotFoundSnafu {
                name: name.to_string(),
            }
            .build())
            .whatever_context("try get tool error");
        };

        // Check Permission
        let granted_once = self.take_once_permission(name, id);
        let granted_session = self.has_session_permission(name, &input);

        if !granted_once && !granted_session {
            // Check if the tool has permission control entries for this session
            if let Some(_pcl) = self.tools_pcl.get(name) {
                // TODO: Implement permission control list validation logic
                unimplemented!("Permission control list validation not yet implemented")
            } else if !matches!(
                name,
                STR_REPLACE_TOOL_NAME | READ_TOOL_NAME | LIST_TOOL_NAME | RUN_TASK_TOOL_NAME
            ) {
                on_output(Output::AskPermission);
                return Ok(ExecuteStatus::Completed);
            };
        }

        if name == BASH_TOOL_NAME {
            let mut envs = self.bash_envs.clone();
            envs.extend(extra_envs_for_bash_input(&input));
            let output = run_bash_chunked(input, &envs, cancel_token.clone(), |chunk| {
                on_output(Output::ToolOutput(chunk.clone()));
            })
            .await;
            on_output(output.into());
            if cancel_token.is_cancelled() {
                return Ok(ExecuteStatus::Cancelled);
            } else {
                return Ok(ExecuteStatus::Completed);
            }
        }

        // Special handling for run_task tool to support streaming SubagentEvents
        if name == RUN_TASK_TOOL_NAME
            && let Some(run_task_tool) = tool
                .as_any()
                .and_then(|any| any.downcast_ref::<RunTaskTool>())
        {
            // Extract the JSON value from input to make it 'static
            let input_value = match input {
                Input::Starter(v) => v,
                _ => {
                    on_output(Output::Failure(Final::from(
                        "run_task requires Starter input",
                    )));
                    return Ok(ExecuteStatus::Completed);
                }
            };

            let ctx = run_task_tool.context();

            // Use a channel to bridge the 'static requirement of run_task
            // with the non-'static on_output callback
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

            // Spawn the run_task future with owned input
            let task_cancel = cancel_token.clone();
            let task_handle = tokio::spawn(async move {
                run_task(
                    ctx,
                    Input::Starter(input_value),
                    task_cancel,
                    move |event| {
                        let _ = event_tx.send(event.clone());
                    },
                )
                .await
            });

            // Pin the task handle outside the loop
            let mut task_handle = std::pin::pin!(task_handle);

            // Forward events while the task runs
            loop {
                tokio::select! {
                    event = event_rx.recv() => {
                        match event {
                            Some(e) => on_output(Output::SubagentOutput(e)),
                            None => {
                                // Channel closed, wait for task to complete and get result
                                match task_handle.await {
                                    Ok(output) => {
                                        on_output(output.into());
                                    }
                                    Err(e) => {
                                        on_output(Output::Failure(Final::from(format!("Task panicked: {}", e))));
                                    }
                                }
                                break;
                            }
                        }
                    }
                    result = &mut task_handle => {
                        // Task completed, drain remaining events
                        while let Ok(e) = event_rx.try_recv() {
                            on_output(Output::SubagentOutput(e));
                        }
                        match result {
                            Ok(output) => {
                                on_output(output.into());
                            }
                            Err(e) => {
                                on_output(Output::Failure(Final::from(format!("Task panicked: {}", e))));
                            }
                        }
                        break;
                    }
                }
            }

            if cancel_token.is_cancelled() {
                return Ok(ExecuteStatus::Cancelled);
            } else {
                return Ok(ExecuteStatus::Completed);
            }
        }

        let output = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Ok(ExecuteStatus::Cancelled);
            }
            output = tool.execute(input) => output,
        };
        if self.auto_accept_edits
            && let Ok(tools::Output::TextEdit(edit)) = output
        {
            on_output(Output::TextEdit(edit.clone()));
            let applied = AppliedTextEdit {
                path: edit.path.as_path(),
                text: edit.new_text.as_str(),
            };
            let applied_output = tool.execute(Input::AppliedTextEdit(applied)).await;
            on_output(applied_output.into());
            return Ok(ExecuteStatus::Completed);
        }
        on_output(output.into());
        Ok(ExecuteStatus::Completed)
    }

    /// Update Permission Control List
    pub fn update_pcl(&mut self, name: &str, pc: PermissionControl) {
        match pc {
            PermissionControl::Once(granted_id) => {
                if let Some(pcl) = self.tools_once_pcl.get_mut(name) {
                    pcl.push(granted_id);
                } else {
                    self.tools_once_pcl
                        .insert(name.to_string(), vec![granted_id]);
                }
            }
            _ => {
                unimplemented!("Permission control {pc:?} is not implemented")
            }
        }
    }

    pub fn grant_session(&mut self, name: &str, input: &serde_json::Value) {
        if name != BASH_TOOL_NAME {
            return;
        }
        let Some(key) = bash_permission_key_from_value(input) else {
            return;
        };
        self.bash_session_allowlist.insert(key);
    }

    fn has_session_permission(&self, name: &str, input: &Input<'_>) -> bool {
        if name != BASH_TOOL_NAME {
            return false;
        }
        let Some(key) = bash_permission_key(input) else {
            return false;
        };
        self.bash_session_allowlist.contains(&key)
    }

    /// Generate a list of tools for provider API
    pub fn provider_tools(&self) -> Vec<crate::provider::Tool> {
        self.tools
            .iter()
            .map(|(name, t)| crate::provider::Tool {
                name: name.to_owned(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    pub fn tool_input_schema(&self, name: &str) -> Option<Value> {
        self.tools.get(name).map(|tool| tool.input_schema())
    }

    fn normalize_tool_names(names: &[String]) -> HashSet<String> {
        names
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{BashOutput, StrReplaceInput};
    use std::collections::HashSet;
    use std::sync::OnceLock;
    use tokio::sync::Mutex;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn bash_input_value(command: &str) -> serde_json::Value {
        serde_json::to_value(BashInput {
            command: command.to_string(),
            timeout: 1_000,
            env: std::collections::BTreeMap::new(),
        })
        .expect("serialize BashInput")
    }

    #[tokio::test]
    async fn bash_session_permission_allows_repeat_command() {
        let mut executor = Executor::default();
        let input_value = bash_input_value("printf 'ok'");

        let mut outputs = Vec::new();
        let _ = executor
            .execute_with_output(
                "1",
                BASH_TOOL_NAME,
                Input::Starter(input_value.clone()),
                CancellationToken::new(),
                |out| outputs.push(out),
            )
            .await
            .expect("execute without session permission");
        assert!(
            outputs
                .iter()
                .any(|out| matches!(out, Output::AskPermission)),
            "expected permission request before granting"
        );

        executor.grant_session(BASH_TOOL_NAME, &input_value);

        let mut outputs = Vec::new();
        let _ = executor
            .execute_with_output(
                "2",
                BASH_TOOL_NAME,
                Input::Starter(input_value),
                CancellationToken::new(),
                |out| outputs.push(out),
            )
            .await
            .expect("execute with session permission");
        assert!(
            !outputs
                .iter()
                .any(|out| matches!(out, Output::AskPermission)),
            "did not expect permission request after granting session"
        );
    }

    #[tokio::test]
    async fn auto_accept_text_edit_applies_changes() {
        let cwd = std::env::current_dir().expect("get cwd");
        let dir = tempfile::Builder::new()
            .prefix("auto-accept")
            .tempdir_in(&cwd)
            .expect("create tempdir");
        let path = dir.path().join("demo.txt");
        tokio::fs::write(&path, "hello\n")
            .await
            .expect("write test file");
        let rel_path = path.strip_prefix(&cwd).expect("strip prefix");

        let input = StrReplaceInput {
            path: rel_path.to_string_lossy().to_string(),
            old_str: "hello\n".to_string(),
            new_str: "world\n".to_string(),
            expected_replacements: 1,
        };
        let input_value = serde_json::to_value(input).expect("serialize input");

        let mut executor = Executor::default();
        executor.set_auto_accept_edits(true);

        let output = executor
            .execute("1", STR_REPLACE_TOOL_NAME, Input::Starter(input_value))
            .await
            .expect("execute str_replace");
        assert!(matches!(output, Output::Success(_)));

        let updated = tokio::fs::read_to_string(&path)
            .await
            .expect("read updated file");
        assert_eq!(updated, "world\n");
    }

    #[tokio::test]
    async fn bash_execution_receives_session_socket_env() {
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().await;
        let previous = std::env::var_os("COCO_SESSION_SOCK");
        // Safety: test-only mutation guarded by ENV_LOCK and restored before return.
        unsafe {
            std::env::set_var("COCO_SESSION_SOCK", "/tmp/coco-executor.sock");
        }

        let mut executor = Executor::default();
        let input_value = bash_input_value(r#"printf "%s" "$COCO_SESSION_SOCK""#);
        executor.update_pcl(
            BASH_TOOL_NAME,
            PermissionControl::Once("inject-test".to_string()),
        );
        let output = executor
            .execute("inject-test", BASH_TOOL_NAME, Input::Starter(input_value))
            .await
            .expect("execute bash with injected env");

        // Safety: test-only mutation guarded by ENV_LOCK.
        unsafe {
            if let Some(value) = previous {
                std::env::set_var("COCO_SESSION_SOCK", value);
            } else {
                std::env::remove_var("COCO_SESSION_SOCK");
            }
        }

        let Output::Success(Final::Json(value)) = output else {
            panic!("expected successful bash output");
        };
        let output: BashOutput =
            serde_json::from_value(value).expect("deserialize bash output json");
        assert_eq!(output.stdout, "/tmp/coco-executor.sock\n");
    }

    #[test]
    fn allowlist_none_keeps_default_tools() {
        let mut executor = Executor::default();
        executor.apply_tool_policies(None, None);
        let names: HashSet<String> = executor
            .provider_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains(BASH_TOOL_NAME));
        assert!(names.contains(READ_TOOL_NAME));
        assert!(names.contains(LIST_TOOL_NAME));
        assert!(names.contains(STR_REPLACE_TOOL_NAME));
    }

    #[test]
    fn allowlist_empty_disables_all_tools() {
        let mut executor = Executor::default();
        executor.apply_tool_policies(Some(&Vec::new()), None);
        let names: Vec<String> = executor
            .provider_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.is_empty());
    }

    #[test]
    fn denylist_removes_tools_from_default() {
        let mut executor = Executor::default();
        executor.apply_tool_policies(
            None,
            Some(&[BASH_TOOL_NAME.to_string(), READ_TOOL_NAME.to_string()]),
        );
        let names: HashSet<String> = executor
            .provider_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(!names.contains(BASH_TOOL_NAME));
        assert!(!names.contains(READ_TOOL_NAME));
        assert!(names.contains(LIST_TOOL_NAME));
        assert!(names.contains(STR_REPLACE_TOOL_NAME));
    }

    #[test]
    fn allowlist_then_denylist_removes_from_allowlist() {
        let mut executor = Executor::default();
        executor.apply_tool_policies(
            Some(&[
                BASH_TOOL_NAME.to_string(),
                READ_TOOL_NAME.to_string(),
                LIST_TOOL_NAME.to_string(),
            ]),
            Some(&[READ_TOOL_NAME.to_string()]),
        );
        let names: HashSet<String> = executor
            .provider_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains(BASH_TOOL_NAME));
        assert!(!names.contains(READ_TOOL_NAME));
        assert!(names.contains(LIST_TOOL_NAME));
        assert!(!names.contains(STR_REPLACE_TOOL_NAME));
    }
}
