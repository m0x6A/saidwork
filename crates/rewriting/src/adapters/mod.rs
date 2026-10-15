mod ollama;
mod openai_chat;

pub(crate) mod http;

pub use ollama::OllamaRewriter;
pub use openai_chat::OpenAiChatRewriter;
