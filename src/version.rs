use std::sync::OnceLock;

static LONG_VERSION: OnceLock<String> = OnceLock::new();
static USER_AGENT: OnceLock<String> = OnceLock::new();

pub fn long_version() -> &'static str {
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

pub fn user_agent() -> &'static str {
    USER_AGENT
        .get_or_init(|| {
            let platform = format!("{}; {}", std::env::consts::OS, std::env::consts::ARCH);
            let dirty = if env!("GIT_DIRTY") == "true" {
                "; dirty"
            } else {
                ""
            };
            format!(
                "code-combo/{} ({platform}) (sha:{}; built:{}{})",
                env!("CARGO_PKG_VERSION"),
                env!("GIT_COMMIT_SHA"),
                env!("BUILT_TIME_UTC"),
                dirty
            )
        })
        .as_str()
}
