use snafu::ResultExt;

use crate::error::*;

pub fn init() -> Result<()> {
    code_combo::logging::init_file_logging("coco-tui")
        .map(|_| ())
        .whatever_context("failed to init logging")
}
