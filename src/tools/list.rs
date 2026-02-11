use async_trait::async_trait;
use indoc::formatdoc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;

use super::{ExecuteResult, Final, Input, Tool, parse_relative_path};

#[derive(Default)]
pub struct ListTool {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListInput {
    pub path: String,
    #[serde(default = "default_entry_limit")]
    pub entry_limit: usize,
}

pub const LIST_TOOL_NAME: &str = "list";
pub const DEFAULT_ENTRY_LIMIT: usize = 1000;
pub const MAX_ENTRY_LIMIT: usize = 1000;
const MAX_BYTES_LIMIT: usize = 1000 * 100; // 100 KB

fn default_entry_limit() -> usize {
    DEFAULT_ENTRY_LIMIT
}

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &'static str {
        LIST_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "List directory entries with permission and type info"
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The relative path from the working directory to the file or directory to list"
                },
                "entry_limit": {
                    "type": "number",
                    "description": formatdoc! {"
                        The maximum number of entries to return.
                        By default list up to {MAX_ENTRY_LIMIT} entries, which is the max allowed value.
                        Set this value when the directory is too large to list at once.
                    "}.trim(),
                    "maximum": MAX_ENTRY_LIMIT,
                    "default": DEFAULT_ENTRY_LIMIT,
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
        let ListInput { path, entry_limit } =
            serde_json::from_value(input).map_err(|err| format!("Invalid input format: {err}"))?;

        if entry_limit > MAX_ENTRY_LIMIT {
            return err_msg!("Exceeded maximum entry limit for listing");
        }

        let path = parse_relative_path(path)?;

        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|err| format!("Failed to read path metadata: {err}"))?;

        let output = if metadata.is_dir() {
            list_dir(&path, entry_limit).await?
        } else {
            list_single(&path, &metadata)?
        };

        Ok(Final::Message(output).into())
    }
}

async fn list_dir(path: &std::path::Path, entry_limit: usize) -> Result<String, Final> {
    let mut rd = fs::read_dir(path)
        .await
        .map_err(|err| format!("Failed to read directory: {err}"))?;

    let mut entries: Vec<EntryInfo> = Vec::new();
    let mut truncated = false;
    while let Some(entry) = rd
        .next_entry()
        .await
        .map_err(|err| format!("Failed to read directory entry: {err}"))?
    {
        if entries.len() >= entry_limit {
            truncated = true;
            break;
        }
        entries.push(EntryInfo::from(entry));
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let mut output = String::new();
    let mut bytes = 0;
    for entry in entries {
        let metadata = fs::symlink_metadata(&entry.path)
            .await
            .map_err(|err| format!("Failed to read entry metadata: {err}"))?;
        let line = format!("{} {}\n", format_mode(&metadata), entry.name);
        output.push_str(&line);
        bytes += line.len();
        if bytes > MAX_BYTES_LIMIT {
            return Err(format!("Exceeded maximum bytes limit for listing: {bytes}").into());
        }
    }

    if truncated {
        let line = format!("... (truncated, showing first {entry_limit} entries)\n");
        output.push_str(&line);
        bytes += line.len();
        if bytes > MAX_BYTES_LIMIT {
            return Err(format!("Exceeded maximum bytes limit for listing: {bytes}").into());
        }
    }

    Ok(output)
}

fn list_single(path: &std::path::Path, metadata: &std::fs::Metadata) -> Result<String, Final> {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    Ok(format!("{} {}\n", format_mode(metadata), name))
}

struct EntryInfo {
    name: String,
    path: std::path::PathBuf,
}

impl From<fs::DirEntry> for EntryInfo {
    fn from(entry: fs::DirEntry) -> Self {
        Self {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path(),
        }
    }
}

#[cfg(unix)]
fn format_mode(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let file_type = metadata.file_type();
    let type_char = if file_type.is_dir() {
        'd'
    } else if file_type.is_file() {
        '-'
    } else if file_type.is_symlink() {
        'l'
    } else if file_type.is_block_device() {
        'b'
    } else if file_type.is_char_device() {
        'c'
    } else if file_type.is_fifo() {
        'p'
    } else if file_type.is_socket() {
        's'
    } else {
        '?'
    };

    let mode = metadata.permissions().mode();
    let mut chars = ['-'; 10];
    chars[0] = type_char;

    let flags = [
        (0o400, 1, 'r'),
        (0o200, 2, 'w'),
        (0o100, 3, 'x'),
        (0o040, 4, 'r'),
        (0o020, 5, 'w'),
        (0o010, 6, 'x'),
        (0o004, 7, 'r'),
        (0o002, 8, 'w'),
        (0o001, 9, 'x'),
    ];
    for (bit, idx, ch) in flags {
        if mode & bit != 0 {
            chars[idx] = ch;
        }
    }

    if mode & 0o4000 != 0 {
        chars[3] = if chars[3] == 'x' { 's' } else { 'S' };
    }
    if mode & 0o2000 != 0 {
        chars[6] = if chars[6] == 'x' { 's' } else { 'S' };
    }
    if mode & 0o1000 != 0 {
        chars[9] = if chars[9] == 'x' { 't' } else { 'T' };
    }

    chars.into_iter().collect()
}

#[cfg(not(unix))]
fn format_mode(metadata: &std::fs::Metadata) -> String {
    let type_char = if metadata.is_dir() { 'd' } else { '-' };
    format!("{type_char}?????????")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn format_mode_for_file_and_dir() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let dir_path = dir.path().join("demo_dir");
        std::fs::create_dir(&dir_path).expect("create dir");
        std::fs::set_permissions(&dir_path, std::fs::Permissions::from_mode(0o755))
            .expect("set dir permissions");

        let file_path = dir.path().join("demo_file");
        std::fs::write(&file_path, "data").expect("write file");
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o640))
            .expect("set file permissions");

        let dir_meta = std::fs::symlink_metadata(&dir_path).expect("dir metadata");
        let file_meta = std::fs::symlink_metadata(&file_path).expect("file metadata");

        assert_eq!(format_mode(&dir_meta), "drwxr-xr-x");
        assert_eq!(format_mode(&file_meta), "-rw-r-----");
    }

    #[test]
    #[cfg(unix)]
    fn format_mode_for_symlink() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let target = dir.path().join("target");
        std::fs::write(&target, "data").expect("write target");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let meta = std::fs::symlink_metadata(&link).expect("link metadata");
        let mode = format_mode(&meta);
        assert!(mode.starts_with('l'));
    }
}
