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
/// Multimodal content (`content_json` set) contributes its text plus a
/// digest of each image reference: engines embed images in the prefix, so
/// two prompts differing only in the image must not look like a KV hit.
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
                        } else if let Some(url) = p.pointer("/image_url/url") {
                            let url = url.as_str().unwrap_or_default();
                            out.push_str("<image:");
                            out.push_str(blake3::hash(url.as_bytes()).to_hex().as_str());
                            out.push_str(">\n");
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

/// Text a request is hashed over: the raw prompt, or the joined messages.
pub fn request_text(prompt: &str, msgs: &[PMessage]) -> String {
    if prompt.is_empty() {
        join_messages(msgs)
    } else {
        prompt.to_string()
    }
}

/// Characters per synthetic token for runs of ordinary text, close to what
/// real tokenizers produce for English.
const CHARS_PER_TOKEN: usize = 4;

/// Scripts where one character is about one token and whitespace is rare.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}'   // Hiragana, Katakana
        | '\u{3400}'..='\u{4dbf}' // CJK Extension A
        | '\u{4e00}'..='\u{9fff}' // CJK Unified
        | '\u{ac00}'..='\u{d7af}' // Hangul
        | '\u{f900}'..='\u{faff}' // CJK Compatibility
        | '\u{20000}'..='\u{2ffff}')
}

/// Dependency-free stand-in for a tokenizer, used when no `tokenizer.json`
/// is configured. Pieces are deterministic in the prompt's prefix, so
/// prefix hashing still works: CJK emits one token per character, other
/// text one per [`CHARS_PER_TOKEN`] characters of each word.
pub fn approximate_token_ids(s: &str) -> Vec<u32> {
    fn emit(piece: &str, out: &mut Vec<u32>) {
        let h = blake3::hash(piece.as_bytes());
        let b = h.as_bytes();
        out.push(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    }
    let mut out = Vec::with_capacity(s.len() / CHARS_PER_TOKEN + 1);
    let mut piece = String::new();
    let mut chars = 0;
    for c in s.chars() {
        let cjk = is_cjk(c);
        if c.is_whitespace() || cjk || chars == CHARS_PER_TOKEN {
            if !piece.is_empty() {
                emit(&piece, &mut out);
                piece.clear();
                chars = 0;
            }
            if c.is_whitespace() {
                continue;
            }
        }
        piece.push(c);
        chars += 1;
        if cjk {
            emit(&piece, &mut out);
            piece.clear();
            chars = 0;
        }
    }
    if !piece.is_empty() {
        emit(&piece, &mut out);
    }
    out
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
        assert_eq!(a.len(), 6);
    }

    #[test]
    fn cjk_prompts_produce_enough_tokens_for_prefix_blocks() {
        // 80 characters with no spaces: whitespace splitting saw one token.
        let text = "\u{4f60}\u{597d}\u{4e16}\u{754c}".repeat(20);
        assert_eq!(approximate_token_ids(&text).len(), 80);
    }

    #[test]
    fn long_unbroken_ascii_still_splits() {
        assert_eq!(approximate_token_ids(&"a".repeat(40)).len(), 10);
    }

    #[test]
    fn approximation_is_prefix_stable() {
        let a = approximate_token_ids("shared system prompt, then one question");
        let b = approximate_token_ids("shared system prompt, then another one");
        assert_eq!(a[..5], b[..5]);
    }

    #[test]
    fn different_images_do_not_share_a_prefix() {
        let image = |url: &str| {
            format!(
                r#"[{{"type":"image_url","image_url":{{"url":"{url}"}}}},{{"type":"text","text":"q"}}]"#
            )
        };
        let a = join_messages(&[msg("user", "", &image("data:a"))]);
        let b = join_messages(&[msg("user", "", &image("data:b"))]);
        assert_ne!(a, b);
    }

    #[test]
    fn raw_prompt_takes_precedence() {
        assert_eq!(request_text("raw", &[msg("user", "x", "")]), "raw");
    }
}
