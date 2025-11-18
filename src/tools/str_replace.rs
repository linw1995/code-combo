use async_trait::async_trait;
use indoc::indoc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
};

use super::{ExecuteResult, Tool};

#[derive(Default)]
pub struct StrReplaceTool {}

#[derive(Debug, Serialize, Deserialize)]
pub struct StrReplaceInput {
    pub path: String,
    pub old_str: String,
    pub new_str: String,
    #[serde(default = "default_expected_replacements")]
    pub expected_replacements: usize,
}

fn default_expected_replacements() -> usize {
    1
}

pub const STR_REPLACE_TOOL_NAME: &str = "str_replace";

#[async_trait]
impl Tool for StrReplaceTool {
    fn name(&self) -> &'static str {
        STR_REPLACE_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Replace str in text files"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path from the working directory to the file to modify"
                },
                "old_str": {
                    "type": "string",
                    "description": indoc!{"
                        The exact literal text to replace, preferably unescaped.
                        For single replacements (default), include at least 3 lines of context BEFORE and AFTER the target text,
                        matching whitespace and indentation precisely.
                        For multiple replacements, specify expected_replacements parameter.
                        If this string is not the exact literal text (i.e. you escaped it) or does not match exactly, the tool will fail.
                        If this string is empty, it means remove all content of the target file or create a new file and write the new_str.
                    "}.trim()
                },
                "new_str": {
                    "type": "string",
                    "description": indoc!{"
                        The exact literal text to replace `old_string` with, preferably unescaped.
                        Provide the EXACT text. Ensure the resulting code is correct and idiomatic.',
                    "}.trim()
                },
                "expected_replacements": {
                    "type": "number",
                    "description": indoc!{"
                        Number of replacements expected.
                        Defaults to 1 if not specified. Use when you want to replace multiple occurrences.
                    "}.trim(),
                    "default": 1,
                    "minimum": 1
                },
            },
            "required": ["path", "old_str", "new_str"]
        })
    }
    async fn execute(&self, input: Value) -> ExecuteResult {
        let StrReplaceInput {
            path,
            old_str,
            new_str,
            expected_replacements,
        } = serde_json::from_value(input)
            .map_err(|err| format!("failed to deserialize tool input: {err}"))?;

        // Check if the path is absolute
        let path = path
            .parse::<std::path::PathBuf>()
            .map_err(|err| format!("failed to parse path: {err}"))?;
        if path.is_absolute() {
            return err_msg!("path must be relative to the working directory");
        }

        if old_str.is_empty() {
            let mut fh = if path.exists() {
                // Overwrite the whole file
                OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .await
                    .map_err(|err| format!("failed to open and truncate file for writing: {err}"))
            } else {
                // Create a new file
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .await
                    .map_err(|err| format!("failed to create file for writing: {err}"))
            }?;
            fh.write_all(new_str.as_bytes())
                .await
                .map_err(|err| format!("failed to write file: {err}"))?;
            fh.flush()
                .await
                .map_err(|err| format!("failed to flush file: {err}"))?;

            Ok("success".into())
        } else {
            // open file
            let fh = tokio::fs::File::open(&path)
                .await
                .map_err(|err| format!("failed to open file: {err}"))?;
            let mut rdr = BufReader::new(fh);

            // read whole file
            let mut text = String::new();
            rdr.read_to_string(&mut text)
                .await
                .map_err(|err| format!("failed to read file: {err}"))?;

            // replace and diff
            let mut new_text = String::with_capacity(
                text.len() + (new_str.len() - old_str.len()) * expected_replacements,
            );

            let mut countdown = expected_replacements;
            let mut offset = 0;
            while countdown > 0
                && let Some(length) = text[offset..].find(&old_str)
            {
                new_text.push_str(&text[offset..offset + length]);
                new_text.push_str(&new_str);
                offset = offset + length + old_str.len();
                countdown -= 1
            }
            if countdown > 0 {
                let found = expected_replacements - countdown;
                return err_msg!(
                    "failed to replace: expected {expected_replacements} replacement(s) but found {found}"
                );
            }

            // write whole file
            let mut fh = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .await
                .map_err(|err| format!("failed to open and truncate file for writing: {err}"))?;
            fh.write_all(new_text.as_bytes())
                .await
                .map_err(|err| format!("failed to write file: {err}"))?;
            fh.flush()
                .await
                .map_err(|err| format!("failed to flush file: {err}"))?;

            Ok("success".into())
        }
    }
}
