use std::{collections::HashMap, sync::Arc};

use lazy_static::lazy_static;
use snafu::prelude::*;
use tokio_util::sync::CancellationToken;

use crate::{
    OutputChunk, TextEdit, error,
    tools::{
        self, BASH_TOOL_NAME, BashInput, BashOutput, BashTool, Final, LIST_TOOL_NAME, ListTool,
        READ_TOOL_NAME, ReadTool, STR_REPLACE_TOOL_NAME, StrReplaceTool, Tool, run_bash_chunked,
    },
};

#[derive(Clone)]
pub struct Executor {
    tools: HashMap<String, Arc<dyn Tool>>,
    tools_pcl: HashMap<String, Vec<(String, PermissionControl)>>,
    tools_once_pcl: HashMap<String, Vec<String>>,
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

impl Executor {
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

        if !granted_once {
            // Check if the tool has permission control entries for this session
            if let Some(_pcl) = self.tools_pcl.get(name) {
                // TODO: Implement permission control list validation logic
                unimplemented!("Permission control list validation not yet implemented")
            } else {
                // Some tools can execute without explicit permission
                if !matches!(
                    name,
                    STR_REPLACE_TOOL_NAME | READ_TOOL_NAME | LIST_TOOL_NAME
                ) {
                    on_output(Output::AskPermission);
                    return Ok(ExecuteStatus::Completed);
                }
            };
        }

        if name == BASH_TOOL_NAME {
            match input {
                Input::Starter(value) => {
                    let parsed: Result<BashInput, _> = serde_json::from_value(value.clone());
                    match parsed {
                        Ok(input) => {
                            let output =
                                match run_bash_chunked(input, cancel_token.clone(), |chunk| {
                                    if cancel_token.is_cancelled() {
                                        return;
                                    }
                                    on_output(Output::ToolOutput(chunk.clone()));
                                })
                                .await
                                {
                                    Ok(output) => output,
                                    Err(err) => {
                                        if cancel_token.is_cancelled() {
                                            return Ok(ExecuteStatus::Cancelled);
                                        }
                                        BashOutput {
                                            exit_code: 255,
                                            stdout: String::new(),
                                            stderr: err,
                                            chunks: Vec::new(),
                                            timed_out: false,
                                        }
                                    }
                                };
                            if cancel_token.is_cancelled() {
                                return Ok(ExecuteStatus::Cancelled);
                            }
                            let is_error = output.exit_code != 0;
                            let final_output = Final::Json(serde_json::to_value(output).unwrap());
                            on_output(if is_error {
                                Output::Failure(final_output)
                            } else {
                                Output::Success(final_output)
                            });
                            return Ok(ExecuteStatus::Completed);
                        }
                        Err(_) => {
                            let output = tokio::select! {
                                _ = cancel_token.cancelled() => {
                                    return Ok(ExecuteStatus::Cancelled);
                                }
                                output = tool.execute(Input::Starter(value)) => output,
                            };
                            on_output(output.into());
                            return Ok(ExecuteStatus::Completed);
                        }
                    }
                }
                _ => {
                    let output = tokio::select! {
                        _ = cancel_token.cancelled() => {
                            return Ok(ExecuteStatus::Cancelled);
                        }
                        output = tool.execute(input) => output,
                    };
                    on_output(output.into());
                    return Ok(ExecuteStatus::Completed);
                }
            }
        }

        let output = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Ok(ExecuteStatus::Cancelled);
            }
            output = tool.execute(input) => output,
        };
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

    /// Generate a list of Tools of Anthropic API
    pub fn anthropic_tools(&self) -> Vec<anthropic::Tool> {
        self.tools
            .iter()
            .map(|(name, t)| anthropic::Tool {
                name: name.to_owned(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }
}
