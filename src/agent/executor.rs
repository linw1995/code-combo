use std::{collections::HashMap, sync::Arc};

use lazy_static::lazy_static;
use snafu::prelude::*;

use crate::{
    TextEdit, error,
    tools::{self, BashTool, Final, ReadTool, STR_REPLACE_TOOL_NAME, StrReplaceTool, Tool},
};

#[derive(Clone)]
pub struct Executor {
    tools: HashMap<String, Arc<dyn Tool>>,
    tools_pcl: HashMap<String, Vec<(String, PermissionControl)>>,
    tools_once_pcl: HashMap<String, Vec<String>>,
}

lazy_static! {
    static ref BASH_TOOL: Arc<(dyn Tool + 'static)> = Arc::new(BashTool::default());
    static ref READ_TOOL: Arc<(dyn Tool + 'static)> = Arc::new(ReadTool::default());
    static ref STR_REPLACE_TOOL: Arc<(dyn Tool + 'static)> = Arc::new(StrReplaceTool::default());
    static ref DEFAULT_TOOLS: HashMap<String, Arc<(dyn Tool + 'static)>> = {
        let mut m = HashMap::<String, Arc<(dyn Tool + 'static)>>::new();
        m.extend(
            [
                BASH_TOOL.clone(),
                READ_TOOL.clone(),
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

#[derive(Debug)]
pub enum Output {
    Success(Final),
    Failure(Final),
    TextEdit(TextEdit),
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
    pub async fn execute<'a>(
        &mut self,
        id: &str,
        name: &str,
        input: Input<'a>,
    ) -> error::Result<Output> {
        // Check tool
        let Some(tool) = self.tools.get(name) else {
            return Err(NotFoundSnafu {
                name: name.to_string(),
            }
            .build())
            .whatever_context("try get tool error");
        };

        // Check Permission
        let granted_once = if let Some(pcl) = self.tools_once_pcl.get_mut(name) {
            let mut granted_idx: Option<usize> = None;
            for (idx, granted) in pcl.iter().enumerate() {
                if granted == id {
                    granted_idx = Some(idx)
                }
            }
            if let Some(idx) = granted_idx {
                pcl.remove(idx);
                true
            } else {
                false
            }
        } else {
            false
        };

        if !granted_once {
            // Check if the tool has permission control entries for this session
            if let Some(_pcl) = self.tools_pcl.get(name) {
                // TODO: Implement permission control list validation logic
                unimplemented!("Permission control list validation not yet implemented")
            } else {
                // Some tools can execute without explicit permission
                if !matches!(name, STR_REPLACE_TOOL_NAME) {
                    // For other tools, request permission if no permission control entries are found
                    return Ok(Output::AskPermission);
                }
            };
        }
        // Just execute the tool
        Ok(tool.execute(input).await.into())
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
