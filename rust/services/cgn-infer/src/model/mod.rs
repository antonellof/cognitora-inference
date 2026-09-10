//! GGUF model loading: mmap the file, parse metadata, extract the
//! embedded tokenizer and chat template.
//!
//! The file is mapped read-only with `memmap2` and Candle reads
//! tensor data through a `Cursor` over the mapping — weights stay
//! page-cache resident, no heap copy of the raw file.

mod chat_template;
pub mod llama;
mod tokenizer;

pub use chat_template::{ChatMessage, ChatTemplate};
pub use llama::{
    split_layers, validate_coverage, LayerRange, ModelParts, QLlama, QLlamaConfig, SeqKv,
};
pub use tokenizer::tokenizer_from_gguf;

use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use candle_core::quantized::gguf_file;
use cgn_core::{Error, Result};
use tracing::info;

/// Metadata extracted from the GGUF header that the rest of the
/// engine cares about.
#[derive(Debug, Clone)]
pub struct ModelMetadata {
    /// `general.architecture`, e.g. "llama".
    pub architecture: String,
    /// `general.name` if present, otherwise the file stem.
    pub name: String,
    /// Training context length (`{arch}.context_length`).
    pub context_length: usize,
    /// BOS / EOS token ids from `tokenizer.ggml.*`.
    pub bos_token_id: Option<u32>,
    pub eos_token_id: Option<u32>,
    /// Raw Jinja chat template (`tokenizer.chat_template`), if embedded.
    pub chat_template: Option<String>,
}

/// A GGUF file mapped into memory with its parsed header.
pub struct GgufModel {
    pub mmap: memmap2::Mmap,
    pub content: gguf_file::Content,
    pub metadata: ModelMetadata,
}

impl GgufModel {
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).map_err(|e| Error::Config(format!("open {}: {e}", path.display())))?;
        // SAFETY: read-only private mapping of a regular file. The file
        // is expected not to be truncated while the server runs (same
        // contract llama.cpp and ds4 rely on).
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|e| Error::Internal(format!("mmap {}: {e}", path.display())))?;

        let mut cursor = Cursor::new(&mmap[..]);
        let content = gguf_file::Content::read(&mut cursor)
            .map_err(|e| Error::Config(format!("gguf parse {}: {e}", path.display())))?;

        let file_stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".to_string());
        let metadata = read_metadata(&content, file_stem)?;
        info!(
            arch = %metadata.architecture,
            name = %metadata.name,
            ctx_train = metadata.context_length,
            tensors = content.tensor_infos.len(),
            size_bytes = mmap.len(),
            "gguf loaded"
        );
        Ok(Self {
            mmap,
            content,
            metadata,
        })
    }

    /// Cursor over the raw mapping, for Candle's tensor loading.
    pub fn cursor(&self) -> Cursor<&[u8]> {
        Cursor::new(&self.mmap[..])
    }
}

fn read_metadata(content: &gguf_file::Content, file_stem: String) -> Result<ModelMetadata> {
    let md = &content.metadata;
    let str_val = |k: &str| md.get(k).and_then(|v| v.to_string().ok()).cloned();
    let u32_val = |k: &str| md.get(k).and_then(|v| v.to_u32().ok());

    let architecture = str_val("general.architecture")
        .ok_or_else(|| Error::Config("gguf: missing general.architecture".into()))?;
    let context_length = md
        .get(&format!("{architecture}.context_length"))
        .and_then(|v| v.to_u32().ok())
        .unwrap_or(4096) as usize;

    Ok(ModelMetadata {
        name: str_val("general.name").unwrap_or(file_stem),
        context_length,
        bos_token_id: u32_val("tokenizer.ggml.bos_token_id"),
        eos_token_id: u32_val("tokenizer.ggml.eos_token_id"),
        chat_template: str_val("tokenizer.chat_template"),
        architecture,
    })
}
