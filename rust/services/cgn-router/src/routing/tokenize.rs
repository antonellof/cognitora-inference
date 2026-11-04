//! Per-model tokenizers for prefix hashing and token counts.
//!
//! The prefix index is only as good as its tokens: real ids line chunks up
//! with what the engine caches, and prompt lengths drive disaggregation
//! and admission. `[models.<name>].tokenizer` points at a `tokenizer.json`;
//! `tokenizer_repo` fetches one from Hugging Face at startup. A model with
//! neither falls back to [`super::prompt::approximate_token_ids`], which
//! the `cgn_router_tokenizer_fallback_total` counter makes visible.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use cgn_core::config::Config;
use cgn_telemetry::counter_vec;
use cgn_telemetry::prometheus::IntCounterVec;
use dashmap::DashMap;
use tokenizers::Tokenizer;
use tracing::{info, warn};

static FALLBACKS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_tokenizer_fallback_total",
        "Requests tokenized with the estimate because no tokenizer is loaded.",
        &["model"]
    )
});

/// Encoding above this size runs off the async workers.
const BLOCKING_ENCODE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct Tokenizers {
    by_model: DashMap<String, Arc<Tokenizer>>,
}

impl Tokenizers {
    /// Load every configured tokenizer. Failures warn and fall back: a
    /// missing tokenizer costs routing accuracy, not availability.
    pub async fn load(cfg: &Config) -> Self {
        let out = Self::default();
        for (model, mc) in &cfg.models {
            let path = match (&mc.tokenizer, &mc.tokenizer_repo) {
                (Some(p), _) => Some(p.clone()),
                (None, Some(repo)) => match fetch(repo).await {
                    Ok(p) => Some(p),
                    Err(e) => {
                        warn!(%model, %repo, error = %e, "tokenizer download failed");
                        None
                    }
                },
                (None, None) => None,
            };
            let Some(path) = path else { continue };
            match Tokenizer::from_file(&path) {
                Ok(t) => {
                    info!(%model, path = %path.display(), "tokenizer loaded");
                    out.insert(model, &mc.lora, t);
                }
                Err(e) => warn!(%model, path = %path.display(), error = %e, "tokenizer unreadable"),
            }
        }
        out
    }

    /// Register `t` for `model` and for the adapters built on it.
    pub fn insert(
        &self,
        model: &str,
        adapters: &std::collections::BTreeMap<String, String>,
        t: Tokenizer,
    ) {
        let t = Arc::new(t);
        self.by_model.insert(model.to_string(), t.clone());
        for adapter in adapters.keys() {
            self.by_model.insert(adapter.clone(), t.clone());
        }
    }

    /// Token ids for `text`, or `None` when no tokenizer is loaded.
    pub async fn encode(&self, model: &str, text: &str) -> Option<Vec<u32>> {
        let t = self.by_model.get(model)?.clone();
        if text.len() < BLOCKING_ENCODE_BYTES {
            return encode_with(&t, text);
        }
        let text = text.to_string();
        tokio::task::spawn_blocking(move || encode_with(&t, &text))
            .await
            .ok()?
    }

    /// Token ids for `text`: real when possible, otherwise the estimate.
    pub async fn token_ids(&self, model: &str, text: &str) -> Vec<u32> {
        match self.encode(model, text).await {
            Some(ids) => ids,
            None => {
                FALLBACKS.with_label_values(&[model]).inc();
                super::prompt::approximate_token_ids(text)
            }
        }
    }
}

fn encode_with(t: &Tokenizer, text: &str) -> Option<Vec<u32>> {
    match t.encode(text, false) {
        Ok(enc) => Some(enc.get_ids().to_vec()),
        Err(e) => {
            warn!(error = %e, "tokenizer encode failed");
            None
        }
    }
}

fn cache_path(repo: &str) -> PathBuf {
    let root = std::env::var_os("CGN_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("cognitora"));
    root.join("tokenizers")
        .join(format!("{}.json", repo.replace('/', "--")))
}

/// Download `tokenizer.json` once; later starts reuse the cached copy.
async fn fetch(repo: &str) -> Result<PathBuf, String> {
    let dest = cache_path(repo);
    if dest.exists() {
        return Ok(dest);
    }
    let base =
        std::env::var("HF_ENDPOINT").unwrap_or_else(|_| "https://huggingface.co".to_string());
    let url = format!("{base}/{repo}/resolve/main/tokenizer.json");
    let mut req = reqwest::Client::new().get(&url);
    if let Ok(token) = std::env::var("HF_TOKEN") {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    write_atomic(&dest, &bytes).map_err(|e| e.to_string())?;
    Ok(dest)
}

fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, dest)
}

#[cfg(test)]
pub(crate) fn test_tokenizer(words: &[&str]) -> Tokenizer {
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;
    let mut vocab: std::collections::HashMap<String, u32> = words
        .iter()
        .enumerate()
        .map(|(i, w)| (w.to_string(), i as u32 + 1))
        .collect();
    vocab.insert("<unk>".into(), 0);
    let model = WordLevel::builder()
        .vocab(vocab.into_iter().collect())
        .unk_token("<unk>".into())
        .build()
        .unwrap();
    let mut t = Tokenizer::new(model);
    t.with_pre_tokenizer(Some(Whitespace {}));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loaded_tokenizer_returns_real_ids() {
        let tk = Tokenizers::default();
        tk.insert(
            "m",
            &Default::default(),
            test_tokenizer(&["hello", "world"]),
        );
        assert_eq!(tk.encode("m", "hello world").await, Some(vec![1, 2]));
        assert_eq!(tk.token_ids("m", "hello world").await, vec![1, 2]);
    }

    #[tokio::test]
    async fn adapters_share_the_base_tokenizer() {
        let tk = Tokenizers::default();
        let lora = [("sql".to_string(), "/lora".to_string())].into();
        tk.insert("m", &lora, test_tokenizer(&["hello"]));
        assert_eq!(tk.encode("sql", "hello").await, Some(vec![1]));
    }

    #[tokio::test]
    async fn unknown_model_falls_back_to_the_estimate() {
        let tk = Tokenizers::default();
        assert_eq!(tk.encode("m", "hello").await, None);
        assert_eq!(tk.token_ids("m", "hello").await.len(), 2);
    }

    #[tokio::test]
    async fn file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tokenizer.json");
        test_tokenizer(&["hello"]).save(&p, false).unwrap();
        let mut cfg = Config::default();
        cfg.models.insert(
            "m".into(),
            cgn_core::config::ModelConfig {
                tokenizer: Some(p),
                ..Default::default()
            },
        );
        let tk = Tokenizers::load(&cfg).await;
        assert_eq!(tk.encode("m", "hello").await, Some(vec![1]));
    }

    #[test]
    fn repo_cache_path_is_flat() {
        assert!(cache_path("Qwen/Qwen3-8B")
            .to_string_lossy()
            .ends_with("tokenizers/Qwen--Qwen3-8B.json"));
    }
}
