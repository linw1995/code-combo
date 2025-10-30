use std::{collections::HashMap, sync::Arc};

use lazy_static::lazy_static;
use serde_json::Value;
use snafu::{Whatever, prelude::*};

use crate::{BashTool, Tool};

#[derive(Clone)]
pub struct Executor {
    tools: HashMap<String, Arc<dyn Tool>>,
}

lazy_static! {
    static ref BASH_TOOL: Arc<(dyn Tool + 'static)> = Arc::new(BashTool::default());
    static ref DEFAULT_TOOLS: HashMap<String, Arc<(dyn Tool + 'static)>> = {
        let mut m = HashMap::<String, Arc<(dyn Tool + 'static)>>::new();
        m.extend(
            [BASH_TOOL.clone()]
                .into_iter()
                .map(|t| (t.name().to_string(), t)),
        );
        m
    };
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            tools: DEFAULT_TOOLS.clone(),
        }
    }
}

#[derive(Snafu, Debug)]
pub enum ExecuteError {
    #[snafu(display("tool {name:?} is not found"))]
    NotFound { name: String },
}

impl Executor {
    pub async fn execute(&self, name: &str, input: Value) -> Result<Value, Whatever> {
        let Some(tool) = self.tools.get(name) else {
            return Err(NotFoundSnafu {
                name: name.to_string(),
            }
            .build())
            .whatever_context("try get tool error");
        };

        tool.execute(input).await
    }
}
