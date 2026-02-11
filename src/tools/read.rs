use async_trait::async_trait;
use indoc::{formatdoc, indoc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{ExecuteResult, Final, Input, Tool, parse_relative_path};

#[derive(Default)]
pub struct ReadTool {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadInput {
    pub path: String,
    #[serde(default = "default_line_offset")]
    pub line_offset: usize,
    #[serde(default = "default_line_limit")]
    pub line_limit: usize,
}

pub const READ_TOOL_NAME: &str = "read";
pub const DEFAULT_LINE_OFFSET: usize = 1;
pub const DEFAULT_LINE_LIMIT: usize = 1000;
pub const MAX_LINE_LIMIT: usize = 1000;
const MAX_BYTES_LIMIT: usize = 1000 * 100; // 100 KB

fn default_line_offset() -> usize {
    DEFAULT_LINE_OFFSET
}

fn default_line_limit() -> usize {
    DEFAULT_LINE_LIMIT
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
                    "default": DEFAULT_LINE_OFFSET,
                    "ge": 1,
                },
                "line_limit": {
                    "type": "number",
                    "description": formatdoc! {"
                        The number of lines to read.
                        By default read up to {MAX_LINE_LIMIT} lines, which is the max allowed value.
                        Set this value when the file is too large to read at once.
                    "}.trim(),
                    "maximum": MAX_LINE_LIMIT,
                    "default": DEFAULT_LINE_LIMIT,
                    "ge": 1,
                }
            },
            "required": ["path"]
        })
    }

    async fn execute<'a>(&self, input: Input<'a>) -> ExecuteResult {
        let Input::Starter(input) = input else {
            return err_msg!("Input should be Starter variant, not other variants");
        };
        let ReadInput {
            path,
            line_offset,
            line_limit,
        } = serde_json::from_value(input).map_err(|err| format!("Invalid input format: {err}"))?;

        if line_limit > MAX_LINE_LIMIT {
            return err_msg!("Exceeded maximum line limit for reading file");
        }

        let path = parse_relative_path(path)?;

        let fh = tokio::fs::File::open(path)
            .await
            .map_err(|err| format!("Failed to open file: {err}"))?;

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
            .map_err(|err| format!("Failed to read line: {err}"))?
        {
            if no >= end {
                break;
            }
            if start <= no {
                output.push_str(&line);
                output.push('\n');

                bytes += line.len() + 1;
                if bytes > MAX_BYTES_LIMIT {
                    return err_msg!("Exceeded maximum bytes limit for reading file: {bytes}");
                }
            }
            no += 1;
        }

        Ok(Final::Message(output).into())
    }
}
