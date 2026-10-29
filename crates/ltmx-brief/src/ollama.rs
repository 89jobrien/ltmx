//! Ollama-backed prose generation.
//!
//! Reuses [`ltmx_embed::ollama::post_json`] rather than hand-rolling a second
//! HTTP/1.1 client: the transport (loopback, chunked-response decoding) is
//! identical to the embedding path, only the endpoint and payload differ.

use crate::BriefError;
use ltmx_embed::ollama::{DEFAULT_HOST, post_json};
use serde::{Deserialize, Serialize};

/// Generates brief prose through a local Ollama `/api/generate` call.
#[derive(Debug, Clone)]
pub struct OllamaGenerator {
    host: String,
    model: String,
}

impl OllamaGenerator {
    /// Builds a generator for `model` on the default loopback host.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            model: model.into(),
        }
    }

    /// Overrides the Ollama host, as `host:port`.
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Sends `prompt` and returns the model's full (non-streamed) reply.
    pub fn generate(&self, prompt: &str) -> Result<String, BriefError> {
        let request = GenerateRequest {
            model: &self.model,
            prompt,
            stream: false,
        };
        let body =
            serde_json::to_string(&request).map_err(|e| BriefError::Decode(e.to_string()))?;

        let (status, response) = post_json(&self.host, "/api/generate", &body)?;
        if status != 200 {
            return Err(BriefError::Status {
                status,
                body: response,
            });
        }

        let parsed: GenerateResponse =
            serde_json::from_str(&response).map_err(|e| BriefError::Decode(e.to_string()))?;
        Ok(parsed.response)
    }
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_host_matches_the_embedder() {
        assert_eq!(OllamaGenerator::new("m").host, DEFAULT_HOST);
    }

    #[test]
    fn with_host_overrides_the_endpoint() {
        let generator = OllamaGenerator::new("m").with_host("10.0.0.5:11434");
        assert_eq!(generator.host, "10.0.0.5:11434");
    }

    #[test]
    fn unreachable_host_reports_unreachable() {
        let generator = OllamaGenerator::new("m").with_host("127.0.0.1:1");
        let err = generator
            .generate("hello")
            .expect_err("port 1 should refuse");
        assert!(matches!(err, BriefError::Unreachable { .. }), "got {err:?}");
    }

    #[test]
    fn request_body_is_valid_json_with_streaming_disabled() {
        let request = GenerateRequest {
            model: "llama3.2",
            prompt: "say hi",
            stream: false,
        };
        let value: serde_json::Value = serde_json::to_string(&request)
            .expect("serialize")
            .parse()
            .expect("json");
        assert_eq!(value["model"], "llama3.2");
        assert_eq!(value["stream"], false);
    }

    #[test]
    fn response_shape_extracts_the_full_text() {
        let response = r#"{"response":"good morning","done":true}"#;
        let parsed: GenerateResponse = serde_json::from_str(response).expect("parse");
        assert_eq!(parsed.response, "good morning");
    }
}
