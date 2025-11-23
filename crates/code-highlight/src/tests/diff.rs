use indoc::indoc;

use crate::{Event::*, Lang, highlight};

const HIGHLIGHT_NAMES: [&str; 11] = [
    "diff.plus",
    "constant",
    "attribute",
    "function",
    "variable.parameter",
    "string.special.path",
    "number",
    "punctuation.special",
    "label",
    "keyword",
    "number",
];

#[test]
fn parse_diff_simple() {
    let diff = indoc! {"
        --- a/file.txt
        +++ b/file.txt
        @@ -1,3 +1,4 @@
         line 1
        -line 2
        +line 2 modified
         line 3
        +line 4
    "};

    let events = highlight(&Lang::Diff, &HIGHLIGHT_NAMES, diff).unwrap();
    assert_eq!(
        events,
        vec![
            Start("punctuation.special"),
            Source("---"),
            End,
            Source(" "),
            Start("string.special.path"),
            Source("a/file.txt"),
            End,
            Source("\n"),
            Start("diff.plus"),
            Start("punctuation.special"),
            Source("+++"),
            End,
            Source(" "),
            Start("string.special.path"),
            Source("b/file.txt"),
            End,
            End,
            Source("\n"),
            Start("attribute"),
            Source("@@ -1,3 +1,4 @@"),
            End,
            Source("\n line 1\n"),
            Start("punctuation.special"),
            Source("-"),
            End,
            Source("line 2\n"),
            Start("diff.plus"),
            Start("punctuation.special"),
            Source("+"),
            End,
            Source("line 2 modified"),
            End,
            Source("\n line 3\n"),
            Start("diff.plus"),
            Start("punctuation.special"),
            Source("+"),
            End,
            Source("line 4"),
            End,
            Source("\n")
        ]
    );
}
