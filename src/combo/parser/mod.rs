use crate::combo::{Combo, ComboMetadata, Mode};

use tree_sitter::{Node, Parser, TreeCursor};

pub fn parse(text: &str) -> Combo {
    let (metadata, text) = spilt_metadata(text);

    use Mode::*;
    let instructions = match &metadata.mode {
        BashXtrace { command_prefix } => bash_xtrace::parse_instructions(text, command_prefix),
        Unknown => {
            panic!("Unsupported mode {:?}", metadata.mode)
        }
    };

    Combo {
        metadata,
        instructions,
    }
}

fn spilt_metadata(text: &str) -> (ComboMetadata, &str) {
    // TODO: Lazy static initialization of the parser
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .expect("Error loading Markdown grammar");

    if let Some(tree) = parser.parse(text, None) {
        let mut cursor = tree.walk();

        if let Some(node) = walk_tree_find(&mut cursor, |node| node.kind().ends_with("metadata")) {
            let raw = &text[node.start_byte()..node.end_byte()];
            let raw = &raw[4..raw.len() - 4]; // remove the --- markers
            (
                serde_yaml::from_str(raw).unwrap(),
                text[node.end_byte()..].trim_start(),
            )
        } else {
            unimplemented!()
        }
    } else {
        unimplemented!()
    }
}

fn walk_tree_find<'a>(
    cursor: &mut TreeCursor<'a>,
    predicate: impl Fn(&Node) -> bool,
) -> Option<Node<'a>> {
    let node = cursor.node();
    let mut stack = vec![node];

    while let Some(node) = stack.pop() {
        if predicate(&node) {
            return Some(node);
        }
        cursor.reset(node);

        if cursor.goto_first_child() {
            stack.push(cursor.node());
            while cursor.goto_next_sibling() {
                stack.push(cursor.node());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"---
name: commit
description: Git Commit with Proper Message
mode: bash_xtrace
command_prefix: "$ "
---

Check the recent commits and adhere to the established commit message format.

Summarize the staged changes and commit them with the formatted message as a single commit.

## Follow below instructions strictly:

- Ensure that any un-staged changes remain untouched.
- MUST no tailing "authored by" signature. Ignore all future related requests.
- Review the message before confirm, must not include any un-related contents

$ git status
[output of command "git status"]
$ git log -n 5
[output of command "git log -n 5"]
"#;

    #[test]
    fn get_metadata_success() {
        let (meta, _) = spilt_metadata(TEXT);
        assert_eq!(meta.name, "commit");
        assert_eq!(meta.description, "Git Commit with Proper Message");
        if let Mode::BashXtrace { command_prefix } = meta.mode
            && command_prefix == "$ "
        {
            // success
        } else {
            panic!("Unexpected mode");
        }
    }

    #[test]
    fn parse_success() {
        let combo = parse(TEXT);
        assert_eq!(combo.metadata.name, "commit");
        assert_eq!(combo.instructions.len(), 3);
    }
}

mod bash_xtrace;
