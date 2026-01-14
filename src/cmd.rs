mod mcp;
mod metadata;
mod prompt;
mod record;

pub use mcp::handle_mcp;
pub use metadata::handle_metadata;
pub use prompt::{handle_ask, handle_reply, handle_tell};
pub use record::handle_record;
