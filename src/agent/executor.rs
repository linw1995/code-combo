use std::{collections::HashMap, sync::Arc};

use lazy_static::lazy_static;
use serde_json::Value;
use snafu::prelude::*;

use crate::{BashTool, Output, ReadTool, StrReplaceTool, Tool, error};

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

#[derive(Debug)]
pub enum ExecuteOutput {
    Success(Output),
    Failure(Output),
    Denied,
    AskPermission,
}

#[derive(Clone, Debug)]
pub enum PermissionControl {
    Once(String),
    Session,
    Always,
}

impl Executor {
    pub async fn execute(
        &mut self,
        id: &str,
        name: &str,
        input: Value,
    ) -> error::Result<ExecuteOutput> {
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

        if granted_once {
            // Just execute the tool
            Ok(match tool.execute(input).await {
                Err(output) => ExecuteOutput::Failure(output),
                Ok(output) => ExecuteOutput::Success(output),
            })
        } else {
            // Check if the tool has permission control entries for this session
            let Some(_pcl) = self.tools_pcl.get(name) else {
                // No permission control entries found, request permission
                return Ok(ExecuteOutput::AskPermission);
            };
            // TODO: Implement permission control list validation logic
            unimplemented!("Permission control list validation not yet implemented")
        }
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
