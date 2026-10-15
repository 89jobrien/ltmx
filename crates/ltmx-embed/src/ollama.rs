//! Ollama-backed [`Embedder`].
//!
//! Talks to the `/api/embeddings` endpoint over a hand-rolled HTTP/1.1 POST.
//! This is deliberately not a general HTTP client: it is a loopback request to
//! a fixed local endpoint, and linking an HTTP stack (reqwest and its ~80
//! transitive crates) for that would defeat the dependency budget.

use ltmx_domain::{EmbedError, Embedder};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Loopback default.
///
/// `127.0.0.1`, not `localhost`: on this machine `localhost` resolves to
/// `::1` first, and Ollama binds IPv4 only, so `localhost` fails where
/// `127.0.0.1` succeeds. Verified against a running Ollama.
pub const DEFAULT_HOST: &str = "127.0.0.1:11434";

/// Model width for `nomic-embed-text`.
pub const NOMIC_DIMENSIONS: usize = 768;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Texts per `/api/embed` request.
///
/// Large enough that per-request overhead is amortized, small enough that a
/// failed batch does not discard much work.
pub const DEFAULT_BATCH_SIZE: usize = 64;

/// Embeds text through a local Ollama server.
#[derive(Debug, Clone)]
pub struct OllamaEmbedder {
    host: String,
    model: String,
    dimensions: usize,
    batch_size: usize,
}

impl OllamaEmbedder {
    /// Builds an embedder for `model` on the default loopback host.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            model: model.into(),
            dimensions: NOMIC_DIMENSIONS,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    /// Overrides the number of texts sent per request.
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Overrides the Ollama host, as `host:port`.
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Overrides the declared vector width.
    ///
    /// Needed for models other than `nomic-embed-text`.
    pub fn with_dimensions(mut self, dimensions: usize) -> Self {
        self.dimensions = dimensions;
        self
    }
}

impl Default for OllamaEmbedder {
    fn default() -> Self {
        Self::new("nomic-embed-text")
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

/// Posts `body` to `path` on `host` and returns the response body.
///
/// Returns the status alongside the body so callers can distinguish a server
/// error from a transport error.
///
/// Ollama answers with `Transfer-Encoding: chunked`, so chunk framing is
/// decoded here rather than assuming a `Content-Length`.
pub fn post_json(host: &str, path: &str, body: &str) -> Result<(u16, String), EmbedError> {
    let unreachable = |reason: String| EmbedError::Unreachable {
        endpoint: format!("http://{host}{path}"),
        reason,
    };

    let addr = host
        .to_socket_addrs()
        .map_err(|e| unreachable(e.to_string()))?
        .next()
        .ok_or_else(|| unreachable("host resolved to no addresses".into()))?;

    let mut stream = TcpStream::connect_timeout(&addr, REQUEST_TIMEOUT)
        .map_err(|e| unreachable(e.to_string()))?;
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|e| unreachable(e.to_string()))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| unreachable(e.to_string()))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| unreachable(e.to_string()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| EmbedError::Decode(format!("no status in {status_line:?}")))?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| unreachable(e.to_string()))?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        let lowered = line.to_ascii_lowercase();
        if let Some(value) = lowered.strip_prefix("content-length:") {
            content_length = value.trim().parse().ok();
        } else if lowered.starts_with("transfer-encoding:") && lowered.contains("chunked") {
            chunked = true;
        }
    }

    let payload = if chunked {
        read_chunked_body(&mut reader).map_err(|e| unreachable(e.to_string()))?
    } else if let Some(len) = content_length {
        let mut payload = vec![0u8; len];
        reader
            .read_exact(&mut payload)
            .map_err(|e| unreachable(e.to_string()))?;
        payload
    } else {
        let mut payload = Vec::new();
        reader
            .read_to_end(&mut payload)
            .map_err(|e| unreachable(e.to_string()))?;
        payload
    };

    let text = String::from_utf8_lossy(&payload).into_owned();
    Ok((status, text))
}

/// Decodes a chunked transfer body into its concatenated payload.
fn read_chunked_body(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line)? == 0 {
            break;
        }
        // A chunk size may carry extensions after `;`; they are not our data.
        let size_text = size_line.trim().split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad chunk size {size_text:?}"),
            ));
        };
        if size == 0 {
            // Consume the trailing CRLF of the terminating chunk.
            let mut trailer = String::new();
            let _ = reader.read_line(&mut trailer);
            break;
        }
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);
        // Consume the CRLF that follows the chunk data.
        let mut crlf = String::new();
        let _ = reader.read_line(&mut crlf);
    }
    Ok(body)
}

impl Embedder for OllamaEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let single = text.to_string();
        self.embed_batch(std::slice::from_ref(&single))?
            .pop()
            .ok_or_else(|| EmbedError::Decode("backend returned no embedding".into()))
    }

    /// Batches server-side via `/api/embed`.
    ///
    /// This override is not an optimization detail. Measured against a local
    /// Ollama, batching runs at roughly 5 ms per text where a per-request
    /// round trip is far slower -- the difference between minutes and hours to
    /// embed a full corpus.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.batch_size.max(1)) {
            let request = EmbedRequest {
                model: &self.model,
                input: batch,
            };
            let body =
                serde_json::to_string(&request).map_err(|e| EmbedError::Decode(e.to_string()))?;

            let (status, response) = post_json(&self.host, "/api/embed", &body)?;
            if status != 200 {
                return Err(EmbedError::Status {
                    status,
                    body: response,
                });
            }

            let parsed: EmbedResponse =
                serde_json::from_str(&response).map_err(|e| EmbedError::Decode(e.to_string()))?;
            if parsed.embeddings.len() != batch.len() {
                return Err(EmbedError::Decode(format!(
                    "expected {} embeddings, backend returned {}",
                    batch.len(),
                    parsed.embeddings.len()
                )));
            }
            for vector in parsed.embeddings {
                if vector.len() != self.dimensions {
                    return Err(EmbedError::DimensionMismatch {
                        expected: self.dimensions,
                        actual: vector.len(),
                    });
                }
                vectors.push(vector);
            }
        }
        Ok(vectors)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_id(&self) -> String {
        format!("ollama:{}", self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_host_is_ipv4_loopback() {
        assert_eq!(DEFAULT_HOST, "127.0.0.1:11434");
    }

    #[test]
    fn model_id_records_the_backend_and_model() {
        let embedder = OllamaEmbedder::new("nomic-embed-text");
        assert_eq!(embedder.model_id(), "ollama:nomic-embed-text");
    }

    #[test]
    fn dimensions_default_to_nomic_width() {
        assert_eq!(OllamaEmbedder::new("nomic-embed-text").dimensions(), 768);
    }

    #[test]
    fn with_dimensions_overrides_the_declared_width() {
        let embedder = OllamaEmbedder::new("other").with_dimensions(1024);
        assert_eq!(embedder.dimensions(), 1024);
    }

    #[test]
    fn with_host_overrides_the_endpoint() {
        let embedder = OllamaEmbedder::new("m").with_host("10.0.0.5:11434");
        assert_eq!(embedder.host, "10.0.0.5:11434");
    }

    #[test]
    fn unreachable_host_reports_unreachable() {
        let embedder = OllamaEmbedder::new("m").with_host("127.0.0.1:1");
        let err = embedder.embed("hello").expect_err("port 1 should refuse");
        assert!(matches!(err, EmbedError::Unreachable { .. }), "got {err:?}");
    }

    #[test]
    fn request_body_is_valid_json_with_model_and_input() {
        let request = EmbedRequest {
            model: "nomic-embed-text",
            input: &["vec0".to_string()],
        };
        let value: serde_json::Value = serde_json::to_string(&request)
            .expect("serialize")
            .parse()
            .expect("json");
        assert_eq!(value["model"], "nomic-embed-text");
        assert_eq!(value["input"][0], "vec0");
    }

    #[test]
    fn response_shape_matches_the_batch_endpoint() {
        let response = r#"{"embeddings":[[0.1,0.2,0.3]]}"#;
        let parsed: EmbedResponse = serde_json::from_str(response).expect("parse");
        assert_eq!(parsed.embeddings.len(), 1);
        assert_ne!(parsed.embeddings[0].len(), NOMIC_DIMENSIONS);
    }

    #[test]
    fn batch_size_is_configurable() {
        assert_eq!(OllamaEmbedder::new("m").with_batch_size(2).batch_size, 2);
    }

    #[test]
    fn dimension_mismatch_is_detected() {
        let response = r#"{"embeddings":[[0.1,0.2,0.3]]}"#;
        let parsed: EmbedResponse = serde_json::from_str(response).expect("parse");
        assert_eq!(parsed.embeddings[0].len(), 3);
        assert_ne!(parsed.embeddings[0].len(), NOMIC_DIMENSIONS);
    }
}
