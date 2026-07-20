//! Embedding-similarity noise filter — the last open M2 item, previously
//! blocked on "an embedding model/service decision" (see the plan's Storage
//! section for the full design: Greptile's disclosed pattern of blocking a
//! new finding similar to ≥N past downvoted comments, passing it through if
//! also similar to ≥N upvoted ones). Resolved here by reusing exactly the
//! pattern already built and live-verified this session for
//! `LocalLlmBackend`: an OpenAI-compatible endpoint (`/v1/embeddings`,
//! llama.cpp's `llama-server --embedding` mode also serves this, same as
//! `/v1/chat/completions`) via `curl`, no new HTTP client dependency.
//!
//! This is real vector-embedding similarity, not the lexical
//! trigram-shingle similarity `report::dedupe` uses for fuzzy dedup/rule
//! clustering — deliberately: the plan's own research distinguishes them
//! (lexical clustering for rule-mining because it must stay explainable to
//! a human reviewer approving a candidate rule; embedding similarity here
//! because catching a *reworded* false-positive pattern, not just a
//! near-identical one, is the actual point of a feedback-similarity filter).

use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// Parses an OpenAI-compatible `/v1/embeddings` response body into the
/// first (and for our single-input requests, only) embedding vector.
pub fn parse_embedding_response(body: &str) -> anyhow::Result<Vec<f32>> {
    let parsed: EmbeddingResponse = serde_json::from_str(body)?;
    parsed.data.into_iter().next().map(|d| d.embedding).ok_or_else(|| anyhow::anyhow!("embeddings response had no data entries"))
}

/// Fetches an embedding vector for `text` from a local OpenAI-compatible
/// server. Same shell-out-via-curl shape as `LocalLlmBackend::invoke` —
/// see that module's docs for why curl over an HTTP client crate.
pub fn fetch_embedding(base_url: &str, model: &str, text: &str, curl_binary: &str) -> anyhow::Result<Vec<f32>> {
    let body = serde_json::json!({ "model": model, "input": text });
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));

    let output = Command::new(curl_binary)
        .args(["-sS", "-X", "POST", &url, "-H", "Content-Type: application/json", "-d", "@-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.take() {
                let mut stdin = stdin;
                let _ = stdin.write_all(body.to_string().as_bytes());
            }
            child.wait_with_output()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("embedding request failed: {}", if stderr.trim().is_empty() { "(no stderr captured)" } else { stderr.trim() });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_embedding_response(&stdout)
}

/// Cosine similarity between two vectors, in `[-1.0, 1.0]` (or `0.0` if
/// either vector is all-zero, avoiding a division by zero rather than
/// producing `NaN`).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
    let norm_a: f64 = a.iter().map(|x| *x as f64 * *x as f64).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| *x as f64 * *x as f64).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Packs an `f32` vector into raw little-endian bytes for storage as a
/// SQLite BLOB — per the plan's Storage section ("Embeddings ... store as a
/// BLOB column; brute-force cosine similarity is fine at this scale").
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Inverse of `embedding_to_bytes`. Returns an error on malformed input
/// (wrong byte length) rather than silently truncating/panicking.
pub fn embedding_from_bytes(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        anyhow::bail!("embedding blob length {} is not a multiple of 4 bytes", bytes.len());
    }
    Ok(bytes.chunks_exact(4).map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_openai_embeddings_response() {
        let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}],"model":"local-embed","usage":{"prompt_tokens":5,"total_tokens":5}}"#;
        let embedding = parse_embedding_response(body).unwrap();
        assert_eq!(embedding, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn errors_clearly_on_malformed_json() {
        assert!(parse_embedding_response("not json").is_err());
    }

    #[test]
    fn errors_clearly_when_data_is_empty() {
        assert!(parse_embedding_response(r#"{"data": []}"#).is_err());
    }

    #[test]
    fn cosine_similarity_is_1_for_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_is_0_for_orthogonal_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_is_negative_for_opposite_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_handles_zero_vectors_without_dividing_by_zero() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn embedding_byte_round_trip_preserves_values() {
        let original = vec![0.5f32, -1.25, 3.0, 0.0, -0.001];
        let bytes = embedding_to_bytes(&original);
        assert_eq!(bytes.len(), original.len() * 4);
        let restored = embedding_from_bytes(&bytes).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn embedding_from_bytes_errors_on_a_length_not_a_multiple_of_four() {
        assert!(embedding_from_bytes(&[1, 2, 3]).is_err());
    }
}
