use std::sync::OnceLock;

static LONG_VERSION: OnceLock<String> = OnceLock::new();
static LONG_VERSIONS: OnceLock<String> = OnceLock::new();

fn long_version() -> &'static str {
    LONG_VERSION
        .get_or_init(|| {
            let dirty = if env!("GIT_DIRTY") == "true" {
                "[dirty]"
            } else {
                ""
            };
            format!(
                "{} (sha:{:?}, build_time:{:?}){}",
                env!("CARGO_PKG_VERSION"),
                env!("GIT_COMMIT_SHA"),
                env!("BUILT_TIME_UTC"),
                dirty
            )
        })
        .as_str()
}

pub fn long_versions() -> &'static str {
    LONG_VERSIONS
        .get_or_init(|| {
            let platform = format!("{}; {}", std::env::consts::OS, std::env::consts::ARCH);
            let coco_tui = long_version();
            let code_combo = code_combo::version::long_version();
            if coco_tui == code_combo {
                coco_tui.to_string()
            } else {
                format!("{coco_tui}\n- code_combo {code_combo}\n- platform: {platform}")
            }
        })
        .as_str()
}
