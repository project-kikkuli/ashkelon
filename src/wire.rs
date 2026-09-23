use serde::{Deserialize, Serialize};

/// The request/response format a call speaks, decided from the request path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    AnthropicMessages,
    OpenAiResponses,
    OpenAiChat,
    /// Anything else (model lists, auth probes): relayed, never parsed or rewritten.
    Opaque,
}

impl Wire {
    pub fn from_path(path: &str) -> Wire {
        let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
        if path.ends_with("/messages") && !path.ends_with("/count_tokens") {
            Wire::AnthropicMessages
        } else if path.ends_with("/responses") {
            Wire::OpenAiResponses
        } else if path.ends_with("/chat/completions") {
            Wire::OpenAiChat
        } else {
            Wire::Opaque
        }
    }
}
