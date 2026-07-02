//! Build a `tokenizers::Tokenizer` from the vocab embedded in a GGUF
//! file, so no sidecar `tokenizer.json` is needed.
//!
//! GGUF embeds one of two tokenizer families (`tokenizer.ggml.model`):
//!
//! * `"llama"` — SentencePiece-style: token strings + scores, spaces
//!   encoded as `▁`, byte fallback via `<0xNN>` tokens. Reconstructed
//!   as a Unigram model.
//! * `"gpt2"`  — byte-level BPE: token strings + merge list.
//!
//! If a `tokenizer.json` sits next to the GGUF we prefer it — it is
//! the exact artifact the model was trained with.

use std::path::Path;

use candle_core::quantized::gguf_file;
use cgn_core::{Error, Result};
use tokenizers::Tokenizer;
use tracing::{debug, info};

pub fn tokenizer_from_gguf(
    content: &gguf_file::Content,
    gguf_path: &Path,
) -> Result<Tokenizer> {
    // Sidecar tokenizer.json wins if present.
    let sidecar = gguf_path.with_file_name("tokenizer.json");
    if sidecar.is_file() {
        info!(path = %sidecar.display(), "using sidecar tokenizer.json");
        return Tokenizer::from_file(&sidecar)
            .map_err(|e| Error::Config(format!("tokenizer.json: {e}")));
    }

    let md = &content.metadata;
    let model = md
        .get("tokenizer.ggml.model")
        .and_then(|v| v.to_string().ok())
        .cloned()
        .ok_or_else(|| Error::Config("gguf: missing tokenizer.ggml.model".into()))?;
    let tokens = string_array(content, "tokenizer.ggml.tokens")?;
    debug!(model = %model, vocab = tokens.len(), "building tokenizer from gguf vocab");

    match model.as_str() {
        "llama" => build_unigram(content, tokens),
        "gpt2" => build_bpe(content, tokens),
        other => Err(Error::Config(format!(
            "unsupported gguf tokenizer model {other:?} (expected \"llama\" or \"gpt2\")"
        ))),
    }
}

fn string_array(content: &gguf_file::Content, key: &str) -> Result<Vec<String>> {
    let vals = content
        .metadata
        .get(key)
        .and_then(|v| v.to_vec().ok())
        .ok_or_else(|| Error::Config(format!("gguf: missing {key}")))?;
    vals.iter()
        .map(|v| {
            v.to_string()
                .cloned()
                .map_err(|e| Error::Config(format!("gguf: {key}: {e}")))
        })
        .collect()
}

/// SentencePiece-style vocab → Unigram model with byte fallback.
fn build_unigram(content: &gguf_file::Content, tokens: Vec<String>) -> Result<Tokenizer> {
    use tokenizers::decoders;
    use tokenizers::models::unigram::Unigram;
    use tokenizers::normalizers;

    let scores: Vec<f32> = content
        .metadata
        .get("tokenizer.ggml.scores")
        .and_then(|v| v.to_vec().ok())
        .map(|vals| {
            vals.iter()
                .map(|v| v.to_f32().unwrap_or(0.0))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![0.0; tokens.len()]);

    let unk_id = content
        .metadata
        .get("tokenizer.ggml.unknown_token_id")
        .and_then(|v| v.to_u32().ok())
        .unwrap_or(0) as usize;

    let vocab: Vec<(String, f64)> = tokens
        .into_iter()
        .zip(scores.into_iter().map(f64::from))
        .collect();
    let model = Unigram::from(vocab, Some(unk_id), true)
        .map_err(|e| Error::Config(format!("gguf unigram vocab: {e}")))?;

    let mut tk = Tokenizer::new(model);
    tk.with_normalizer(Some(normalizers::Sequence::new(vec![
        normalizers::Prepend::new("▁".into()).into(),
        normalizers::Replace::new(" ", "▁")
            .map_err(|e| Error::Config(format!("normalizer: {e}")))?
            .into(),
    ])));
    tk.with_decoder(Some(decoders::sequence::Sequence::new(vec![
        // `Replace` doubles as a decoder in the tokenizers crate.
        normalizers::Replace::new("▁", " ")
            .map_err(|e| Error::Config(format!("decoder: {e}")))?
            .into(),
        decoders::byte_fallback::ByteFallback::new().into(),
        decoders::fuse::Fuse::new().into(),
        decoders::strip::Strip::new(' ', 1, 0).into(),
    ])));
    Ok(tk)
}

/// Byte-level BPE vocab + merges (GPT-2 family, incl. Llama 3).
fn build_bpe(content: &gguf_file::Content, tokens: Vec<String>) -> Result<Tokenizer> {
    use tokenizers::decoders::byte_level::ByteLevel as ByteLevelDec;
    use tokenizers::models::bpe::BpeBuilder;
    use tokenizers::pre_tokenizers::byte_level::ByteLevel;

    let merges_raw = string_array(content, "tokenizer.ggml.merges")?;
    let merges: Vec<(String, String)> = merges_raw
        .iter()
        .filter_map(|m| {
            m.split_once(' ')
                .map(|(a, b)| (a.to_string(), b.to_string()))
        })
        .collect();

    let vocab = tokens
        .into_iter()
        .enumerate()
        .map(|(i, t)| (t, i as u32))
        .collect();
    let model = BpeBuilder::new()
        .vocab_and_merges(vocab, merges)
        .ignore_merges(true)
        .build()
        .map_err(|e| Error::Config(format!("gguf bpe vocab: {e}")))?;

    let mut tk = Tokenizer::new(model);
    tk.with_pre_tokenizer(Some(ByteLevel::new(false, false, true)));
    tk.with_decoder(Some(ByteLevelDec::new(false, false, true)));
    Ok(tk)
}
