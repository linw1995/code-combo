use anthropic_api::messages::Message;

#[allow(dead_code)]
pub struct Session {
    // Use the Message struct from the anthropic_api crate for now
    pub messages: Vec<Message>,
}
