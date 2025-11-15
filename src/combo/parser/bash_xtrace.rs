//! Parser for bash xtrace output format.
//!
//! This module provides functionality to parse bash shell script execution traces
//! generated with `set -x` (xtrace) option enabled. The parser processes the output
//! where each command is prefixed with a customizable command prefix (typically "$ ")
//! and followed by its output lines.
//!
//! # Usage
//!
//! The parser is typically used with bash scripts that have:
//! - `PS4='$ '` to set the command prefix
//! - `BASH_XTRACEFD=1` to redirect xtrace output
//! - `set -x` to enable command tracing
//! - `set +x` to disable command tracing (optional)
//!
//! # Configuration
//!
//! The `command_prefix` parameter is typically configured through markdown metadata
//! in the combo file. The metadata is specified in YAML format within `---` delimiters
//! at the beginning of the file:
//!
//! ```markdown
//! ---
//! name: commit
//! description: Git Commit with Proper Message
//! mode: bash_xtrace
//! command_prefix: "$ "
//! ---
//! ```
//!
//! # Example Input Format
//!
//! ```text
//! $ git status
//! On branch main
//! Your branch is up to date with 'origin/main'.
//!
//! $ git log -n 2
//! commit abc123
//! Author: John Doe <john@example.com>
//! Date:   Mon Jan 1 12:00:00 2024 +0000
//!
//!     Initial commit
//! ```
//!
//! # Output
//!
//! The parser converts the input into a sequence of `Instruction` variants:
//! - `Instruction::Command { command, output }` for command lines and their outputs
//! - `Instruction::Text(text)` for any text blocks that don't follow command patterns

use crate::combo::Instruction;

/// Parses bash xtrace output into a sequence of instructions.
///
/// This function processes text containing bash command traces where each command
/// is prefixed with the specified `command_prefix` and followed by its output lines.
///
/// The `command_prefix` parameter is typically extracted from markdown metadata
/// in the combo file, which specifies the parsing mode and configuration.
///
/// # Arguments
///
/// * `text` - The input text containing bash xtrace output
/// * `command_prefix` - The prefix that identifies command lines (e.g., "$ ")
///
/// # Returns
///
/// A vector of `Instruction` variants representing the parsed structure:
/// - Command instructions with their associated output
/// - Text instructions for non-command content
///
/// # Example
///
/// ```rust
/// use crate::combo::Instruction;
/// use crate::combo::parser::bash_xtrace::parse_instructions;
///
/// let input = "$ echo hello\nhello\n$ echo world\nworld\n";
/// let instructions = parse_instructions(input, "$ ");
///
/// assert_eq!(instructions.len(), 2);
/// if let Instruction::Command { command, output } = &instructions[0] {
///     assert_eq!(command, "echo hello");
///     assert_eq!(output, "hello\n");
/// }
/// ```
pub fn parse_instructions(text: &str, command_prefix: &str) -> Vec<Instruction> {
    let mut instructions: Vec<Instruction> = vec![];

    // Helper macro to push text blocks as instructions
    // If the last instruction is a command with empty output, append to its output
    // Otherwise, create a new Text instruction
    macro_rules! push_text {
        ($block:expr) => {
            if let Some(last) = instructions.last_mut()
                && let Instruction::Command { output, .. } = last
                && output.is_empty()
            {
                output.push_str(&$block);
            } else {
                instructions.push(Instruction::Text($block.trim_end().to_string()))
            }
        };
    }

    // Process each line in the input text
    let last = text.lines().fold(String::new(), |mut block, line| {
        // Check if the line starts with the command prefix
        if let Some(command) = line.strip_prefix(command_prefix) {
            // Push accumulated text block (if any) before processing the command
            push_text!(block);

            // Skip bash set commands (used to enable/disable xtrace mode)
            if !command.trim_start().starts_with("set") {
                // Create a new command instruction with the parsed command
                let command = command.trim().to_string();
                let inst = Instruction::Command {
                    command,
                    output: String::new(),
                };
                instructions.push(inst);
            }

            // Reset the block for accumulating output
            String::new()
        } else {
            // Accumulate non-command lines as output for the previous command
            block.push_str(line);
            block.push('\n');
            block
        }
    });

    // Push any remaining text after processing all lines
    push_text!(last);

    instructions
}
