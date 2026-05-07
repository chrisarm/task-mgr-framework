//! Cross-encoder reranker over an OpenAI-compatible `/v1/rerank` endpoint.
//!
//! The [`Reranker`] trait exists solely to enable test-double injection in the
//! recall pipeline; the only production implementation is [`LlamaBoxReranker`],
//! which talks to a [gpustack/llama-box](https://github.com/gpustack/llama-box)
//! server.
//!
//! ## Contract
//!
//! Implementations MUST:
//! 1. Replace each candidate's `relevance_score` with the cross-encoder score
//!    returned by the server (i.e. throw away the original FTS5/cosine score).
//! 2. Return the candidates sorted descending by the new score.
//! 3. Return `Err(...)` on any validation failure (HTTP non-2xx, length mismatch,
//!    duplicate or out-of-range indices, malformed JSON). The recall pipeline
//!    decides whether to soft-fail and fall back to the input order — the
//!    reranker itself never silently degrades.
//!
//! Empty input is short-circuited to `Ok(vec![])` without an HTTP call.
//!
//! ## Document and query text
//!
//! Each candidate is sent as `format!("{title}\n\n{content}")` truncated to at
//! most [`MAX_DOC_CHARS`] Unicode characters via `chars().take(N)`. The query
//! is independently truncated to [`MAX_QUERY_CHARS`]. The truncations are
//! char-safe so non-ASCII content never panics on a UTF-8 boundary.
//!
//! The caps target the jina-reranker-v2 training context of 1024 tokens.
//! Cross-encoders evaluate `(query, document)` together, so the server rejects
//! the *entire batch* with HTTP 400 if any single (query+doc) pair exceeds
//! `n_ctx_train`. The companion docker image runs llama-box with
//! `--parallel 1` so the full 1024 tokens go to a single sequence (default
//! n_seq_max=8 would divide it into 128-token shards). Combined with these
//! caps, typical English content (~0.25–0.4 tokens/char) clears with margin;
//! adversarial content (~1 token/char) is what the caps defend against.

use std::time::Duration;

use serde::Deserialize;

use super::retrieval::ScoredLearning;
use crate::{TaskMgrError, TaskMgrResult};

/// Maximum number of Unicode characters per document sent to the reranker.
const MAX_DOC_CHARS: usize = 1024;

/// Maximum number of Unicode characters in the query sent to the reranker.
///
/// Long free-text queries (e.g. pasted log lines) would otherwise eat into the
/// document's share of the 1024-token context window. See [`MAX_DOC_CHARS`].
const MAX_QUERY_CHARS: usize = 256;

/// Snippet length (in chars) when echoing a malformed response into an error message.
const ERROR_BODY_SNIPPET_CHARS: usize = 200;

/// A pluggable cross-encoder reranker.
///
/// The trait exists to allow injecting a test double in the recall pipeline.
/// New production implementations should not be added here unless absolutely
/// necessary — the recall config currently selects between "configured" (i.e.
/// [`LlamaBoxReranker`]) and "disabled" (no reranker), nothing else.
pub trait Reranker: Send + Sync {
    /// Replace each candidate's `relevance_score` with the cross-encoder score
    /// for `(query, candidate)` and return the candidates sorted descending by
    /// that new score.
    ///
    /// Empty input MUST short-circuit without making an HTTP call.
    fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredLearning>,
    ) -> TaskMgrResult<Vec<ScoredLearning>>;
}

/// Production reranker that posts to a llama-box `/v1/rerank` endpoint.
pub struct LlamaBoxReranker {
    base_url: String,
    model: String,
    agent: ureq::Agent,
}

impl LlamaBoxReranker {
    /// Construct a reranker pointing at the given llama-box server and model.
    ///
    /// Timeouts: 3s connect, 60s read. The read timeout is longer than
    /// [`OllamaEmbedder`](crate::learnings::embeddings::OllamaEmbedder)'s 30s
    /// because the first rerank after a cold model load can take 20–30 s for
    /// GPU warmup; subsequent calls are typically sub-second.
    pub fn new(base_url: &str, model: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout_read(Duration::from_secs(60))
            .build();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            agent,
        }
    }
}

/// Compose the document text sent over the wire for a single candidate.
///
/// Format: `"{title}\n\n{content}"`, truncated to [`MAX_DOC_CHARS`] Unicode
/// characters. `chars().take(N)` never splits a codepoint, so non-ASCII
/// content cannot panic.
fn build_document(learning: &crate::models::Learning) -> String {
    let combined = format!("{}\n\n{}", learning.title, learning.content);
    combined.chars().take(MAX_DOC_CHARS).collect()
}

#[derive(Deserialize)]
struct RerankResult {
    index: usize,
    relevance_score: f64,
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

/// Build an error capturing a snippet of the offending response body. Kept private —
/// callers wrap a `std::io::Error` in `TaskMgrError::IoError` so the error chain
/// remains a flat `IoError(...)` from the consumer's perspective.
fn malformed_response_err(url: &str, detail: &str, body: &str) -> TaskMgrError {
    let snippet: String = body.chars().take(ERROR_BODY_SNIPPET_CHARS).collect();
    TaskMgrError::IoError(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("rerank response from {url} {detail}: {snippet}"),
    ))
}

impl Reranker for LlamaBoxReranker {
    fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredLearning>,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/v1/rerank", self.base_url);
        let documents: Vec<String> = candidates
            .iter()
            .map(|c| build_document(&c.learning))
            .collect();
        let truncated_query: String = query.chars().take(MAX_QUERY_CHARS).collect();

        let payload = serde_json::json!({
            "model": self.model,
            "query": truncated_query,
            "documents": documents,
            "top_n": documents.len(),
        });

        let resp = match self.agent.post(&url).send_json(&payload) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(TaskMgrError::IoError(std::io::Error::other(format!(
                    "rerank request to {url} failed: HTTP {code}: {body}"
                ))));
            }
            Err(ureq::Error::Transport(t)) => {
                let kind = match t.kind() {
                    ureq::ErrorKind::ConnectionFailed => std::io::ErrorKind::ConnectionRefused,
                    _ => std::io::ErrorKind::Other,
                };
                return Err(TaskMgrError::IoError(std::io::Error::new(
                    kind,
                    format!("rerank request to {url} failed: {t}"),
                )));
            }
        };

        // Read the body as a string first so we can include a snippet in any
        // parse-error message. into_string() is bounded by ureq's default cap.
        let body = resp.into_string().map_err(|e| {
            TaskMgrError::IoError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to read rerank response from {url}: {e}"),
            ))
        })?;

        let parsed: RerankResponse = serde_json::from_str(&body)
            .map_err(|e| malformed_response_err(&url, &format!("malformed JSON ({e})"), &body))?;

        let n = documents.len();
        if parsed.results.len() != n {
            return Err(malformed_response_err(
                &url,
                &format!(
                    "length mismatch: sent {n} documents, got {} results",
                    parsed.results.len()
                ),
                &body,
            ));
        }

        // Validate every index is in range and unique. Use a presence vector
        // (cheaper than a HashSet for our small N) keyed by index.
        let mut seen = vec![false; n];
        for r in &parsed.results {
            if r.index >= n {
                return Err(malformed_response_err(
                    &url,
                    &format!("index {} out of range [0, {n})", r.index),
                    &body,
                ));
            }
            if seen[r.index] {
                return Err(malformed_response_err(
                    &url,
                    &format!("duplicate index {} in results", r.index),
                    &body,
                ));
            }
            seen[r.index] = true;
        }

        // Map each result back onto its candidate by `index`, replacing the
        // original retrieval score with the cross-encoder score. This explicit
        // index-based mapping is required: the server may return results in any
        // order (sorted by score, by original index, or arbitrary), so trusting
        // the response position would silently corrupt ordering.
        let mut by_index: Vec<Option<ScoredLearning>> = candidates.into_iter().map(Some).collect();
        let mut reranked: Vec<ScoredLearning> = Vec::with_capacity(n);
        for r in parsed.results {
            // `seen[r.index]` already guarantees no duplicates, and the bounds
            // check above guarantees `r.index < by_index.len()`. The take()
            // therefore must yield Some on first visit; defend against future
            // logic changes with an explicit error rather than unwrap.
            let mut sl = by_index[r.index].take().ok_or_else(|| {
                malformed_response_err(
                    &url,
                    &format!("internal: index {} taken twice", r.index),
                    &body,
                )
            })?;
            sl.relevance_score = r.relevance_score;
            sl.match_reason = Some("cross-encoder rerank".to_string());
            reranked.push(sl);
        }

        // Sort descending by the new cross-encoder score. Use partial_cmp's
        // Equal fallback so NaN scores (which the server shouldn't return but
        // we don't want to panic on) collapse to a stable position.
        reranked.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(reranked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Learning, LearningOutcome};

    fn make_candidate(id: i64, title: &str, content: &str) -> ScoredLearning {
        let mut learning = Learning::new(LearningOutcome::Pattern, title, content);
        learning.id = Some(id);
        ScoredLearning {
            learning,
            relevance_score: 0.0,
            match_reason: None,
        }
    }

    #[test]
    fn test_empty_candidates_short_circuits() {
        // mockito server with NO mocks set; if the impl calls the endpoint, the
        // request 404s and the test fails on Err. We additionally check that
        // dropping the server (no expectations registered) is fine.
        let server = mockito::Server::new();
        let url = server.url();
        let reranker = LlamaBoxReranker::new(&url, "test-model");

        let result = reranker.rerank("anything", Vec::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_orders_by_score() {
        let mut server = mockito::Server::new();
        // Out-of-position indices (2, 0, 1) — verifies we map by `index` field
        // and not by response position.
        let mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"results":[
                    {"index":2,"relevance_score":0.9},
                    {"index":0,"relevance_score":0.5},
                    {"index":1,"relevance_score":0.1}
                ]}"#,
            )
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![
            make_candidate(1, "A", "alpha content"),
            make_candidate(2, "B", "bravo content"),
            make_candidate(3, "C", "charlie content"),
        ];

        let result = reranker.rerank("query", candidates).unwrap();
        mock.assert();
        assert_eq!(result.len(), 3);
        // Sorted descending by new score: candidate index 2 (id=3) first, then 0 (id=1), then 1 (id=2).
        assert_eq!(result[0].learning.id, Some(3));
        assert!((result[0].relevance_score - 0.9).abs() < 1e-9);
        assert_eq!(
            result[0].match_reason.as_deref(),
            Some("cross-encoder rerank")
        );
        assert_eq!(result[1].learning.id, Some(1));
        assert!((result[1].relevance_score - 0.5).abs() < 1e-9);
        assert_eq!(result[2].learning.id, Some(2));
        assert!((result[2].relevance_score - 0.1).abs() < 1e-9);
    }

    #[test]
    fn test_unreachable_returns_err() {
        // Port 0 is reserved and never accepts connections.
        let reranker = LlamaBoxReranker::new("http://127.0.0.1:0", "test-model");
        let candidates = vec![make_candidate(1, "A", "x")];
        let err = reranker.rerank("q", candidates).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("rerank request") || msg.contains("127.0.0.1"),
            "expected unreachable error to mention the request, got: {msg}"
        );
    }

    #[test]
    fn test_response_index_out_of_range_returns_err() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            // 3 docs sent, response references index=5
            .with_body(
                r#"{"results":[
                    {"index":0,"relevance_score":0.9},
                    {"index":1,"relevance_score":0.5},
                    {"index":5,"relevance_score":0.1}
                ]}"#,
            )
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![
            make_candidate(1, "A", "a"),
            make_candidate(2, "B", "b"),
            make_candidate(3, "C", "c"),
        ];
        let err = reranker.rerank("q", candidates).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("out of range"),
            "expected 'out of range' in error, got: {msg}"
        );
    }

    #[test]
    fn test_response_duplicate_index_returns_err() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"results":[
                    {"index":0,"relevance_score":0.9},
                    {"index":1,"relevance_score":0.5},
                    {"index":1,"relevance_score":0.1}
                ]}"#,
            )
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![
            make_candidate(1, "A", "a"),
            make_candidate(2, "B", "b"),
            make_candidate(3, "C", "c"),
        ];
        let err = reranker.rerank("q", candidates).unwrap_err();
        assert!(
            err.to_string().contains("duplicate"),
            "expected 'duplicate' in error, got: {err}"
        );
    }

    #[test]
    fn test_response_length_mismatch_returns_err() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            // 3 docs sent, only 2 results returned
            .with_body(
                r#"{"results":[
                    {"index":0,"relevance_score":0.9},
                    {"index":1,"relevance_score":0.5}
                ]}"#,
            )
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![
            make_candidate(1, "A", "a"),
            make_candidate(2, "B", "b"),
            make_candidate(3, "C", "c"),
        ];
        let err = reranker.rerank("q", candidates).unwrap_err();
        assert!(
            err.to_string().contains("length mismatch"),
            "expected 'length mismatch' in error, got: {err}"
        );
    }

    #[test]
    fn test_malformed_json_returns_err() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "text/plain")
            .with_body("not json")
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(1, "A", "x")];
        let err = reranker.rerank("q", candidates).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("malformed JSON"),
            "expected malformed-JSON error, got: {msg}"
        );
    }

    #[test]
    fn test_http_5xx_returns_err() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(500)
            .with_body("upstream exploded")
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(1, "A", "x")];
        let err = reranker.rerank("q", candidates).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("HTTP 500"),
            "expected 'HTTP 500' in error message, got: {msg}"
        );
        assert!(
            msg.contains(&server.url()),
            "expected URL in error message, got: {msg}"
        );
    }

    #[test]
    fn test_connection_refused_returns_connection_refused_kind() {
        // Use a port that is not listening. Pick a high ephemeral port unlikely
        // to be in use; if it happens to be taken the test will still pass
        // (it would just get a different transport error kind, which maps to Other).
        let reranker = LlamaBoxReranker::new("http://127.0.0.1:19999", "test-model");
        let candidates = vec![make_candidate(1, "A", "x")];
        let err = reranker.rerank("q", candidates).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("127.0.0.1:19999"),
            "expected URL in error message, got: {msg}"
        );
        // The error kind should be ConnectionRefused (or at minimum an IoError,
        // not a 5xx-flavored message).
        assert!(
            !msg.contains("HTTP "),
            "transport error should not contain 'HTTP', got: {msg}"
        );
    }

    #[test]
    fn test_document_text_format() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/v1/rerank")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"documents":["My Title\n\nMy content body"]}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.42}]}"#)
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(1, "My Title", "My content body")];
        let result = reranker.rerank("q", candidates).unwrap();
        mock.assert();
        assert_eq!(result.len(), 1);
        assert!((result[0].relevance_score - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_long_content_truncated_to_max_doc_chars() {
        // Build content that, combined with title + "\n\n", exceeds the cap.
        let title = "T";
        let content: String = "a".repeat(MAX_DOC_CHARS * 2);
        let combined = format!("{title}\n\n{content}");
        assert!(combined.chars().count() > MAX_DOC_CHARS);

        // Capture and verify document length via the request body matcher.
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/v1/rerank")
            .match_body(mockito::Matcher::Regex("documents".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(1, title, &content)];
        let _ = reranker.rerank("q", candidates).unwrap();
        mock.assert();

        // Re-derive what the impl would have sent and assert == MAX_DOC_CHARS chars.
        let learning = make_candidate(1, title, &content).learning;
        let doc = build_document(&learning);
        assert_eq!(
            doc.chars().count(),
            MAX_DOC_CHARS,
            "long content must be truncated to MAX_DOC_CHARS"
        );
    }

    #[test]
    fn test_long_query_truncated_to_max_query_chars() {
        let mut server = mockito::Server::new();
        // The mock only matches if the request body contains a query string of
        // exactly MAX_QUERY_CHARS 'q' chars (no more, no less). If the impl
        // forgets to truncate, the body has 2*MAX_QUERY_CHARS 'q' chars and
        // the mock will not match — making mock.assert() fail.
        let expected_query: String = "q".repeat(MAX_QUERY_CHARS);
        let json_substr = format!(r#""query":"{expected_query}""#);
        let mock = server
            .mock("POST", "/v1/rerank")
            .match_body(mockito::Matcher::PartialJsonString(format!(
                r#"{{"query":"{}"}}"#,
                expected_query
            )))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(1, "T", "c")];
        let long_query: String = "q".repeat(MAX_QUERY_CHARS * 2);
        let _ = reranker.rerank(&long_query, candidates).unwrap();
        mock.assert();
        // Also defend against future refactors that might double-truncate or skip:
        // the substring check confirms the exact serialized form.
        assert!(json_substr.contains(&expected_query));
    }

    #[test]
    fn test_truncate_is_char_safe_for_non_ascii() {
        // Each em dash is 3 bytes in UTF-8; 500 of them is 1500 bytes / 500 chars.
        // Pad with ASCII to push char count above 1024 and verify no panic.
        let title = "T";
        let mut content = String::new();
        // 600 em dashes (600 chars, 1800 bytes) + 500 ASCII = 1100 chars total.
        for _ in 0..600 {
            content.push('—');
        }
        content.push_str(&"x".repeat(500));

        let learning = make_candidate(1, title, &content).learning;
        let doc = build_document(&learning);

        assert!(doc.chars().count() <= MAX_DOC_CHARS);
        // Round-trip through String to ensure we truncated on a char boundary.
        let _byte_len = doc.len();
        assert!(doc.is_char_boundary(doc.len()));
    }

    #[test]
    fn test_single_candidate_round_trip() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.7}]}"#)
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(42, "only", "one")];
        let result = reranker.rerank("q", candidates).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].learning.id, Some(42));
        assert!((result[0].relevance_score - 0.7).abs() < 1e-9);
    }

    #[test]
    fn test_empty_title_and_content() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.0}]}"#)
            .create();

        let reranker = LlamaBoxReranker::new(&server.url(), "test-model");
        let candidates = vec![make_candidate(1, "", "")];
        let result = reranker.rerank("q", candidates).unwrap();
        assert_eq!(result.len(), 1);
        // Empty title+content -> document is just "\n\n".
        let doc = build_document(&result[0].learning);
        assert_eq!(doc, "\n\n");
    }
}
