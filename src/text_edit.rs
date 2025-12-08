use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct TextEdit {
    pub path: PathBuf,
    pub origin: Arc<String>,
    pub text: String,
    pub new_text: String,
}

impl fmt::Debug for TextEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextEdit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

pub struct AppliedTextEdit<'a> {
    pub path: &'a Path,
    pub text: &'a str,
}

impl fmt::Debug for AppliedTextEdit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppliedTextEdit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl TextEdit {
    pub fn new(path: PathBuf, text: String, new_text: String) -> Self {
        Self {
            path,
            origin: Arc::new(text.clone()),
            text,
            new_text,
        }
    }

    pub fn update(&self, text: String) -> Self {
        Self {
            path: self.path.clone(),
            origin: Arc::clone(&self.origin),
            text,
            new_text: self.new_text.clone(),
        }
    }

    pub fn update_new(&self, new_text: String) -> Self {
        Self {
            path: self.path.clone(),
            origin: Arc::clone(&self.origin),
            text: self.text.clone(),
            new_text,
        }
    }

    pub fn changed(&self) -> bool {
        *self.origin != self.text
    }

    pub fn text_diff<'a>(&'a self) -> similar::TextDiff<'a, 'a, 'a, str> {
        similar::TextDiff::from_lines(&self.text, &self.new_text)
    }

    pub fn reject_hunk(&self, context_radius: usize, idx: usize) -> Option<Self> {
        let diff = self.text_diff();
        let hunk = diff
            .unified_diff()
            .context_radius(context_radius)
            .iter_hunks()
            .nth(idx)?;

        let ops = hunk.ops();
        let (first, last) = (ops[0], ops[ops.len() - 1]);

        let old_start = first.old_range().start;
        let new_start = first.new_range().start;
        let old_end = last.old_range().end;
        let new_end = last.new_range().end;

        /*
         * Reject Hunk - Index Mapping Diagram
         *
         * SOURCE LINES:
         * text:    ...────[old_start]─────────[old_end]────...
         *                    \                       /
         *                     \                     /
         *                      \                   /
         * new_text:    ...────[new_start]──hunk──[new_end]────...
         *
         * RESULT CONSTRUCTION:
         * new_text = new_text[0..new_start] + text[old_start..old_end] + new_text[new_end..]
         *            <--- from new_text ---> <--- from text ---> <--- from new_text --->
         */

        let mut new_text = String::with_capacity(self.new_text.len());
        self.new_text
            .split('\n') // Use split('\n') instead of String::lines() to preserve original line content
            .take(new_start)
            .chain(
                self.text
                    .split('\n')
                    .skip(old_start)
                    .take(old_end - old_start),
            )
            .chain(self.new_text.split('\n').skip(new_end))
            .for_each(|line| {
                new_text.push_str(line);
                new_text.push('\n');
            });
        new_text.pop(); // Remove the last newline character
        new_text.shrink_to_fit();

        if new_text == self.text {
            None
        } else {
            Some(self.update_new(new_text))
        }
    }

    pub fn apply_hunk<'a>(
        &'a mut self,
        context_radius: usize,
        idx: usize,
    ) -> Option<(AppliedTextEdit<'a>, Option<Self>)> {
        let diff = self.text_diff();
        let hunk = diff
            .unified_diff()
            .context_radius(context_radius)
            .iter_hunks()
            .nth(idx)?;

        let ops = hunk.ops();
        let (first, last) = (ops[0], ops[ops.len() - 1]);

        let old_start = first.old_range().start;
        let new_start = first.new_range().start;
        let old_end = last.old_range().end;
        let new_end = last.new_range().end;

        /*
         * Apply Hunk - Index Mapping Diagram
         *
         * SOURCE LINES (current state):
         * text:       ...────[old_start]─────────[old_end]────...
         *                    \                       /
         *                     \                     /
         *                      \                   /
         * new_text:   ...────[new_start]──hunk──[new_end]────...
         *
         * RESULT CONSTRUCTION:
         * applied_text = text[0..old_start] + new_text[new_start..new_end] + text[old_end..]
         *                <--- from text ---> <--- from new_text ---> <--- from text --->
         *
         * This only applies the current hunk without affecting previous hunks.
         */

        let mut applied_text = String::with_capacity(self.text.len() + self.new_text.len());
        self.text
            .split('\n') // Use split('\n') instead of String::lines() to preserve original line content
            .take(old_start)
            .chain(
                self.new_text
                    .split('\n')
                    .skip(new_start)
                    .take(new_end - new_start),
            )
            .chain(self.text.split('\n').skip(old_end))
            .for_each(|line| {
                applied_text.push_str(line);
                applied_text.push('\n');
            });
        applied_text.pop(); // Remove the last newline character
        applied_text.shrink_to_fit();

        let finished = applied_text == self.new_text;
        self.text = applied_text;
        let applied = AppliedTextEdit {
            path: self.path.as_path(),
            text: &self.text,
        };
        Some(if finished {
            (applied, None)
        } else {
            (applied, Some(self.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use similar::DiffOp::*;
    use tracing::debug;

    use super::*;

    #[test]
    fn text_diff() {
        let edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Equal {
                    old_index: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let diff = edit.text_diff();
        assert_eq!(
            diff.ops(),
            [
                Replace {
                    old_index: 0,
                    old_len: 1,
                    new_index: 0,
                    new_len: 1
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 1
                },
                Insert {
                    old_index: 2,
                    new_index: 2,
                    new_len: 1
                },
                Equal {
                    old_index: 2,
                    new_index: 3,
                    len: 1
                },
                Replace {
                    old_index: 3,
                    old_len: 1,
                    new_index: 4,
                    new_len: 1
                },
                Equal {
                    old_index: 4,
                    new_index: 5,
                    len: 1
                }
            ],
            "Text diff should correctly identify the differences between original and new text"
        )
    }

    #[test]
    fn apply_hunk() {
        let mut edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Equal {
                    old_index: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let diff = edit.text_diff();
        let hunk = diff
            .unified_diff()
            .iter_hunks()
            .next()
            .expect("should have at least one hunk");
        assert_eq!(
            hunk.ops(),
            [
                Replace {
                    old_index: 0,
                    old_len: 1,
                    new_index: 0,
                    new_len: 1
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 1
                },
                Insert {
                    old_index: 2,
                    new_index: 2,
                    new_len: 1
                },
                Equal {
                    old_index: 2,
                    new_index: 3,
                    len: 1
                },
                Replace {
                    old_index: 3,
                    old_len: 1,
                    new_index: 4,
                    new_len: 1
                },
                Equal {
                    old_index: 4,
                    new_index: 5,
                    len: 1
                }
            ],
            "First hunk should contain all diff operations since there's only one hunk"
        );
        let new_text = edit.new_text.clone();
        let (applied, rest) = edit
            .apply_hunk(3, 0)
            .expect("should successfully apply first hunk");
        debug!(?applied.text, ?rest, "apply change result");
        assert!(
            rest.is_none(),
            "Should return None for rest since all changes are applied in one hunk"
        );
        assert_eq!(
            applied.text, &new_text,
            "Applied text should match the target new_text after applying all changes"
        );
    }

    #[test]
    fn apply_hunks() {
        let mut edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Delete {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Equal {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let diff = edit.text_diff();
        assert_eq!(
            diff.ops(),
            [
                Replace {
                    old_index: 0,
                    new_index: 0,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 13
                },
                Replace {
                    old_index: 14,
                    new_index: 14,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 15,
                    new_index: 15,
                    len: 1
                },
            ],
            "Text diff should identify two Replace operations with Equal content in between"
        );
        let hunk = diff
            .unified_diff()
            .context_radius(3)
            .iter_hunks()
            .next()
            .expect("should have at least one hunk");
        assert_eq!(
            hunk.ops(),
            [
                Replace {
                    old_index: 0,
                    new_index: 0,
                    old_len: 1,
                    new_len: 1
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 3
                }
            ],
            "First hunk should contain the first Replace operation with 3 lines of context"
        );
        let new_text = edit.new_text.clone();
        let (applied, new_edit) = edit
            .apply_hunk(3, 0)
            .expect("should successfully apply first hunk");
        assert_eq!(
            applied.text,
            indoc! {"
                Equal {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim(),
            "Applied text should contain the first line change but not the complete transformation"
        );
        assert_ne!(
            applied.text, &new_text,
            "Applied text should not equal final new_text since only first hunk is applied"
        );
        let new_edit = new_edit.expect("should return Some(new_edit) for remaining changes");
        assert_eq!(
            applied.text, new_edit.text,
            "New edit should use the applied text as its base text"
        );
        assert_eq!(
            new_text, new_edit.new_text,
            "New edit should still aim for the original target new_text"
        );
    }

    #[test]
    fn reject_hunk() {
        let edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Delete {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Equal {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let diff = edit.text_diff();
        assert_eq!(
            diff.ops(),
            [
                Replace {
                    old_index: 0,
                    new_index: 0,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 13
                },
                Replace {
                    old_index: 14,
                    new_index: 14,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 15,
                    new_index: 15,
                    len: 1
                },
            ],
            "Text diff should identify two Replace operations separated by equal content"
        );
        let hunk = diff
            .unified_diff()
            .context_radius(3)
            .iter_hunks()
            .next()
            .expect("should have at least one hunk");
        assert_eq!(
            hunk.ops(),
            [
                Replace {
                    old_index: 0,
                    new_index: 0,
                    old_len: 1,
                    new_len: 1
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 3
                }
            ],
            "First hunk should contain first Replace operation with 3 lines of context"
        );
        let new_edit = edit
            .reject_hunk(3, 0)
            .expect("should successfully reject first hunk");
        assert_eq!(
            new_edit.text, edit.text,
            "Rejected hunk should preserve the original text"
        );
        assert_ne!(
            new_edit.new_text, edit.new_text,
            "Rejected hunk should modify the new_text to exclude the hunk changes"
        );
        assert_eq!(
            &new_edit.new_text,
            indoc! {"
            Delete {
                old_index: usize,
                old_len: usize,
                new_index: usize,
            },
            Insert {
                old_index: usize,
                new_index: usize,
                new_len: usize,
            },
            Replace {
                old_index: usize,
                old_len: usize,
                new_index: usize,
                new_len: usize,
            },
        "}
            .trim(),
            "Rejected hunk should restore the first line back to original text"
        );

        let edit = new_edit;
        let new_edit = edit.reject_hunk(3, 0);
        assert!(
            new_edit.is_none(),
            "Should return None when there are no more hunks to reject"
        );
        assert!(
            !edit.changed(),
            "All hunks are rejected, so it remains unchanged"
        )
    }

    #[test]
    fn mix_hunk_actions() {
        let mut edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Delete {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Equal {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let (_, new_edit) = edit
            .apply_hunk(3, 0)
            .expect("should successfully apply the first hunk");
        let edit = new_edit.expect("Should be Some when there is one more hunk");
        let new_edit = edit.reject_hunk(3, 0);
        assert!(
            new_edit.is_none(),
            "Should return None when there are no more hunks to act"
        );
        assert!(
            edit.changed(),
            "One hunk was applied, so it becomes changed"
        );
    }

    #[test]
    fn reject_last_hunk() {
        let edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Delete {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Equal {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let diff = edit.text_diff();
        assert_eq!(
            diff.ops(),
            [
                Replace {
                    old_index: 0,
                    new_index: 0,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 13
                },
                Replace {
                    old_index: 14,
                    new_index: 14,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 15,
                    new_index: 15,
                    len: 1
                },
            ],
            "Text diff should identify two Replace operations separated by equal content"
        );
        let hunk = diff
            .unified_diff()
            .context_radius(3)
            .iter_hunks()
            .nth(1)
            .expect("should have at least two hunks for rejecting the last one");
        assert_eq!(
            hunk.ops(),
            [
                Equal {
                    old_index: 11,
                    new_index: 11,
                    len: 3
                },
                Replace {
                    old_index: 14,
                    new_index: 14,
                    old_len: 1,
                    new_len: 1
                },
                Equal {
                    old_index: 15,
                    new_index: 15,
                    len: 1
                }
            ],
            "Second hunk should contain the last Replace operation with context"
        );
        let new_edit = edit
            .reject_hunk(3, 1)
            .expect("should successfully reject the last hunk");
        assert_eq!(
            new_edit.text, edit.text,
            "Rejected last hunk should preserve the original text"
        );
        assert_ne!(
            new_edit.new_text, edit.new_text,
            "Rejected last hunk should modify the new_text to exclude the last hunk changes"
        );
        assert_eq!(
            &new_edit.new_text,
            indoc! {"
            Equal {
                old_index: usize,
                old_len: usize,
                new_index: usize,
            },
            Insert {
                old_index: usize,
                new_index: usize,
                new_len: usize,
            },
            Replace {
                old_index: usize,
                old_len: usize,
                new_index: usize,
                len: usize,
            },
        "}
            .trim(),
            "Rejected last hunk should remove the last line change, keeping the first line as target"
        )
    }

    #[test]
    fn apply_last_hunk() {
        let mut edit = TextEdit::new(
            "./example.rs".parse().unwrap(),
            indoc! {"
                Delete {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    len: usize,
                },
            "}
            .trim()
            .to_string(),
            indoc! {"
                Equal {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
            .to_string(),
        );
        let diff = edit.text_diff();
        assert_eq!(
            diff.ops(),
            [
                Replace {
                    old_index: 0,
                    new_index: 0,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 1,
                    new_index: 1,
                    len: 13
                },
                Replace {
                    old_index: 14,
                    new_index: 14,
                    old_len: 1,
                    new_len: 1,
                },
                Equal {
                    old_index: 15,
                    new_index: 15,
                    len: 1
                },
            ]
        );
        let hunk = diff
            .unified_diff()
            .context_radius(3)
            .iter_hunks()
            .nth(1)
            .expect("should have at least two hunk");
        assert_eq!(
            hunk.ops(),
            [
                Equal {
                    old_index: 11,
                    new_index: 11,
                    len: 3
                },
                Replace {
                    old_index: 14,
                    new_index: 14,
                    old_len: 1,
                    new_len: 1
                },
                Equal {
                    old_index: 15,
                    new_index: 15,
                    len: 1
                }
            ]
        );
        let (applied, rest) = edit.apply_hunk(3, 1).expect("should success");
        assert_eq!(
            applied.text,
            indoc! {"
                Delete {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                },
                Insert {
                    old_index: usize,
                    new_index: usize,
                    new_len: usize,
                },
                Replace {
                    old_index: usize,
                    old_len: usize,
                    new_index: usize,
                    new_len: usize,
                },
            "}
            .trim()
        );
        assert!(
            rest.is_some(),
            "Should return Some(rest) since not all hunks are applied"
        );

        let rest_edit = rest.unwrap();
        assert_eq!(
            rest_edit.text, applied.text,
            "The rest edit should have the applied text as its base"
        );
        assert_eq!(
            rest_edit.new_text, edit.new_text,
            "But still aim for the original new_text"
        );
    }
}
