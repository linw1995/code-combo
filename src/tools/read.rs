use async_trait::async_trait;
use indoc::{formatdoc, indoc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{ExecuteResult, Output, Tool};

#[derive(Default)]
pub struct ReadTool {}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReadInput {
    pub path: String,
    #[serde(default = "default_line_offset")]
    pub line_offset: usize,
    #[serde(default = "default_line_limit")]
    pub line_limit: usize,
}

pub const READ_TOOL_NAME: &str = "read";
const MAX_LINE_LIMIT: usize = 1000;
const MAX_BYTES_LIMIT: usize = 1000 * 100; // 100 KB

fn default_line_offset() -> usize {
    1
}

fn default_line_limit() -> usize {
    MAX_LINE_LIMIT
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        READ_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Read text files"
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path from the working directory to the file to read"
                },
                "line_offset": {
                    "type": "number",
                    "description": indoc! {"
                        The line number to start reading from.
                        By default read from the beginning of the file.
                        Set this when the file is too large to read at once.
                    "}
                    .trim(),
                    "default": 1,
                    "ge": 1,
                },
                "line_limit": {
                    "type": "number",
                    "description": formatdoc! {"
                        The number of lines to read.
                        By default read up to {MAX_LINE_LIMIT} lines, which is the max allowed value.
                        Set this value when the file is too large to read at once.
                    "}.trim(),
                    "max": MAX_LINE_LIMIT,
                    "default": MAX_LINE_LIMIT,
                    "ge": 1,
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: Value) -> ExecuteResult {
        let ReadInput {
            path,
            line_offset,
            line_limit,
        } = serde_json::from_value(input)
            .map_err(|err| format!("failed to deserialize tool input: {err}"))?;

        if line_limit > MAX_LINE_LIMIT {
            return err_msg!("exceeded maximum line limit for reading file");
        }

        // Check if the path is absolute
        let path = path
            .parse::<std::path::PathBuf>()
            .map_err(|err| format!("failed to parse path: {err}"))?;
        if path.is_absolute() {
            return err_msg!("path must be relative to the working directory");
        }

        let fh = tokio::fs::File::open(path)
            .await
            .map_err(|err| format!("failed to open file: {err}"))?;

        let mut rdr = BufReader::new(fh).lines();

        // Convert to zero-based index.
        let start = line_offset - 1;
        let end = start + line_limit;

        let mut no = 0;
        let mut bytes = 0;
        let mut output = String::new();
        while let Some(line) = rdr
            .next_line()
            .await
            .map_err(|err| format!("failed to read line: {err}"))?
        {
            if no >= end {
                break;
            }
            if start <= no {
                output.push_str(&line);
                output.push('\n');

                bytes += line.len() + 1;
                if bytes > MAX_BYTES_LIMIT {
                    return err_msg!("exceeded max bytes limit of reading file: {bytes}");
                }
            }
            no += 1;
        }

        Ok(Output::Message(output))
    }
}
