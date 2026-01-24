use std::io::{self, Write};

const OSC_PREFIX: &str = "\x1b]9;";
const OSC_SUFFIX: &str = "\x07";
const MAX_TITLE_LEN: usize = 64;
const MAX_BODY_LEN: usize = 160;

pub fn send_osc9(title: &str, body: &str) {
    let Some(seq) = build_osc9(title, body) else {
        return;
    };
    let mut stdout = io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

fn build_osc9(title: &str, body: &str) -> Option<String> {
    let title = sanitize_field(title, MAX_TITLE_LEN);
    let body = sanitize_field(body, MAX_BODY_LEN);
    if title.is_empty() && body.is_empty() {
        return None;
    }
    let payload = if !title.is_empty() && !body.is_empty() {
        format!("{title};{body}")
    } else if !title.is_empty() {
        title
    } else {
        body
    };
    Some(format!("{OSC_PREFIX}{payload}{OSC_SUFFIX}"))
}

fn sanitize_field(value: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(value.len().min(max_len));
    let mut prev_space = false;
    for mut ch in value.chars() {
        if ch.is_control() {
            if matches!(ch, '\n' | '\r' | '\t') {
                ch = ' ';
            } else {
                continue;
            }
        }
        if ch == ';' {
            ch = ':';
        }
        if ch == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        if out.len() + ch.len_utf8() > max_len {
            break;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_builds_payload_with_title_and_body() {
        let seq = build_osc9("coco", "Reply ready").expect("sequence");
        assert!(seq.starts_with(OSC_PREFIX));
        assert!(seq.ends_with(OSC_SUFFIX));
        let payload = &seq[OSC_PREFIX.len()..seq.len() - OSC_SUFFIX.len()];
        assert_eq!(payload, "coco;Reply ready");
    }

    #[test]
    fn osc9_sanitizes_control_chars_and_semicolons() {
        let seq = build_osc9("co;co", "line1\nline2\x1b[31m").expect("sequence");
        let payload = &seq[OSC_PREFIX.len()..seq.len() - OSC_SUFFIX.len()];
        assert_eq!(payload, "co:co;line1 line2[31m");
    }
}
