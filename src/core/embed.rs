//! Text embeddings via model2vec static embeddings (model2vec-rs).
//!
//! No transformer forward pass: a token->vector lookup plus mean pooling. The
//! model (~30 MB) loads in well under a second and runs on CPU. Used by the
//! indexer to embed every chunk and by search to embed the query, with the SAME
//! model so vectors are comparable.
//!
//! potion is symmetric — unlike bge, there is NO query instruction prefix.
//! Output vectors are L2-normalized (per the model config), so cosine == dot
//! product.

use std::sync::Mutex;

use anyhow::Result;
use model2vec_rs::model::StaticModel;
use once_cell::sync::OnceCell;

use crate::core::constants::embed_model;

// Lazily load the model once per process and reuse it. Guarded by a Mutex so it
// is `Sync` (the MCP server may call embed from a blocking thread pool).
// Downloaded from Hugging Face on first use and cached in the HF cache.
static MODEL: OnceCell<Mutex<StaticModel>> = OnceCell::new();

fn with_model<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&StaticModel) -> R,
{
    let cell = MODEL.get_or_try_init(|| -> Result<Mutex<StaticModel>> {
        // normalize=None => honor the model config (potion normalizes).
        let model = StaticModel::from_pretrained(embed_model(), None, None, None)?;
        Ok(Mutex::new(model))
    })?;
    let guard = cell.lock().expect("embedding model mutex poisoned");
    Ok(f(&guard))
}

/// Embed passages (documents). Returns one float32 vector per input.
pub fn embed_passages(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    with_model(|m| m.encode(texts))
}

/// Embed a single search query (no prefix for potion).
pub fn embed_query(query: &str) -> Result<Vec<f32>> {
    with_model(|m| m.encode_single(query))
}

/// Pack an embedding into the little-endian float32 blob sqlite-vec expects
/// (matches numpy `<f4` bytes).
pub fn to_vec_blob(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for f in vec {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}
