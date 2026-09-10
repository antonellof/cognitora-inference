//! Prompt flattening and approximate tokenisation for prefix hashing.
//!
//! Shared by every routing entry point (OpenAI HTTP gateway, router gRPC
//! surface, embeddings) so all paths compute *identical* prefix hashes
//! for the same prompt. Before 0.7 the gateway and the gRPC surface each
//! carried a private copy of this logic and they drifted: the gRPC copy
//! ignored multimodal `content_json`, so the two paths disagreed on the
//! prefix digests of the same multimodal request.

use cgn_proto::v1::Message as PMessage;

/// Join chat-style messages into one stable string for prefix-hashing
/// purposes. Stable across versions; not the model's chat template.
///
/// Multimodal content (`content_json` set) contributes only its textual
/// parts: image bytes don't affect prompt-prefix KV reuse in the engines
/// we route to, and hashing megabyte-scale data URLs would be pure
/// overhead.
pub fn join_messages(msgs: &[PMessage]) -> String {
    let mut out = String::with_capacity(msgs.iter().map(|m| m.content.len() + 16).sum());
    for m in msgs {
        out.push('<');
        out.push_str(&m.role);
        out.push_str(">\n");
        if !m.content_json.is_empty() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&m.content_json) {
                if let Some(parts) = v.as_array() {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                }
            }
        } else {
            out.push_str(&m.content);
        }
        out.push('\n');
    }
    out
}

/// Quick, dependency-free approximation: split on whitespace and hash
/// each word to a synthetic token id. Used only when the full tokenizer
/// hasn't been resolved for the model yet (cold start). The real path
/// uses `tokenizers::Tokenizer::encode`.
pub fn approximate_token_ids(s: &str) -> Vec<u32> {
    s.split_whitespace()
        .map(|w| {
            let h = blake3::hash(w.as_bytes());
            let b = h.as_bytes();
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str, content_json: &str) -> PMessage {
        PMessage {
            role: role.into(),
            content: content.into(),
            name: String::new(),
            content_json: content_json.into(),
            tool_calls_json: String::new(),
            tool_call_id: String::new(),
        }
    }

    #[test]
    fn plain_and_multimodal_text_hash_alike() {
        // A plain-text message and a single-text-part multimodal message
        // carry the same prompt text, so their joined forms must agree on
        // that text (framing newlines aside).
        let plain = join_messages(&[msg("user", "hello world", "")]);
        let mm = join_messages(&[msg("user", "", r#"[{"type":"text","text":"hello world"}]"#)]);
        assert!(plain.contains("hello world"));
        assert!(mm.contains("hello world"));
    }

    #[test]
    fn image_parts_do_not_contribute() {
        let mm = join_messages(&[msg(
            "user",
            "",
            r#"[{"type":"text","text":"caption this"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]"#,
        )]);
        assert!(mm.contains("caption this"));
        assert!(!mm.contains("base64"));
    }

    #[test]
    fn token_ids_are_deterministic() {
        let a = approximate_token_ids("the quick brown fox");
        let b = approximate_token_ids("the quick brown fox");
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
    }
}
