pub(crate) fn preferred_temp_dir() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("COCO_TEST_TMPDIR") {
        let path = std::path::PathBuf::from(path);
        if path.is_dir() {
            return path;
        }
    }
    let system = std::env::temp_dir();
    if cfg!(unix) {
        let short = std::path::PathBuf::from("/tmp");
        if short.is_dir() && short.to_string_lossy().len() < system.to_string_lossy().len() {
            return short;
        }
    }
    system
}
