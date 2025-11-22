use async_trait::async_trait;
use indoc::indoc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
};

use super::{ExecuteResult, Final, Input, Output, TextEdit, Tool};
use crate::AppliedTextEdit;

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
    async fn execute<'a>(&self, input: Input<'a>) -> ExecuteResult {
        match input {
            Input::Starter(input) => self.prepare(input).await.map(Output::from),
            Input::AppliedTextEdit(input) => self.apply_edit(input).await,
        }
    }
}

impl StrReplaceTool {
    async fn prepare(&self, input: Value) -> Result<TextEdit, Final> {
        let StrReplaceInput {
            path,
            old_str,
            new_str,
            expected_replacements,
        } = serde_json::from_value(input).map_err(|err| format!("Invalid input format: {err}"))?;

        // Check if the path is absolute
        let path = path
            .parse::<std::path::PathBuf>()
            .map_err(|err| format!("Invalid path format: {err}"))?;

        if path.is_absolute() {
            return Err("Path must be relative to the working directory, not absolute".into());
        }

        if old_str.is_empty() {
            Ok(TextEdit::new(path, String::new(), new_str.to_string()))
        } else {
            // open file
            let fh = tokio::fs::File::open(&path)
                .await
                .map_err(|err| format!("Failed to open file: {err}"))?;
            let mut rdr = BufReader::new(fh);

            // read whole file
            let mut text = String::new();
            rdr.read_to_string(&mut text)
                .await
                .map_err(|err| format!("Failed to read file: {err}"))?;

            // replace
            let new_text = self.replace(&text, &old_str, &new_str, expected_replacements)?;

            Ok(TextEdit::new(path, text, new_text))
        }
    }

    async fn apply_edit<'a>(&self, input: AppliedTextEdit<'a>) -> ExecuteResult {
        // TODO: Use writing temporary file then renaming for safe editing.
        // This ensures atomic file updates and prevents data loss if the operation is interrupted.
        let AppliedTextEdit { path, text } = input;

        // Truncate (or create) and rewrite the entire file
        let mut fh = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(&path)
            .await
            .map_err(|err| format!("Failed to open file for writing: {err}"))?;
        fh.write_all(text.as_bytes())
            .await
            .map_err(|err| format!("Failed to write file content: {err}"))?;
        fh.flush()
            .await
            .map_err(|err| format!("Failed to flush file changes: {err}"))?;

        Ok(Final::from("Success").into())
    }

    fn replace(
        &self,
        text: &str,
        old_str: &str,
        new_str: &str,
        expected_replacements: usize,
    ) -> Result<String, Final> {
        let mut new_text = String::with_capacity(
            text.len() + new_str.len() * expected_replacements
                - old_str.len() * expected_replacements,
        );

        let mut countdown = expected_replacements;
        let mut offset = 0;
        while countdown > 0
            && let Some(length) = text[offset..].find(old_str)
        {
            new_text.push_str(&text[offset..offset + length]);
            new_text.push_str(new_str);
            offset = offset + length + old_str.len();
            countdown -= 1
        }
        if countdown > 0 {
            let found = expected_replacements - countdown;
            Err(format!(
                "failed to replace: expected {expected_replacements} replacement(s) but found {found}"
            ).into())
        } else {
            Ok(new_text)
        }
    }
}
