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

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use model2vec_rs::model::StaticModel;
use once_cell::sync::OnceCell;

use crate::core::constants::{embed_model, models_dir, MODEL_FILES};

// Lazily load the model once per process and reuse it. Guarded by a Mutex so it
// is `Sync` (the MCP server may call embed from a blocking thread pool).
// URL models are downloaded once into the data dir (see `resolve_model`);
// Hugging Face ids are fetched into the HF cache by model2vec-rs.
static MODEL: OnceCell<Mutex<StaticModel>> = OnceCell::new();

/// Whether a model spec is an `http(s)://` folder URL (e.g. a GitHub release).
pub fn is_url_model(spec: &str) -> bool {
    spec.starts_with("https://") || spec.starts_with("http://")
}

/// Local cache folder for a URL model: `<data dir>/models/<sanitised url>`.
pub fn url_model_dir(spec: &str) -> PathBuf {
    let name: String = spec
        .split_once("://")
        .map_or(spec, |(_, rest)| rest)
        .trim_end_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    models_dir().join(name)
}

/// Turn the configured model spec into something model2vec-rs can load: URL
/// models are downloaded (once) into the data dir and loaded from there; Hugging
/// Face ids and local paths pass through unchanged.
pub fn resolve_model(spec: &str) -> Result<String> {
    if !is_url_model(spec) {
        return Ok(spec.to_string());
    }
    let dir = url_model_dir(spec);
    if MODEL_FILES.iter().all(|f| dir.join(f).is_file()) {
        return Ok(dir.to_string_lossy().into_owned());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    eprintln!("[sndoc] downloading embedding model from {spec} (one-time)...");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    for f in MODEL_FILES {
        let dest = dir.join(f);
        if dest.is_file() {
            continue;
        }
        let url = format!("{}/{f}", spec.trim_end_matches('/'));
        let mut resp = client
            .get(&url)
            .send()
            .with_context(|| format!("downloading {url}"))?;
        if !resp.status().is_success() {
            bail!("downloading {url}: HTTP {}", resp.status());
        }
        // Write to a per-process temp file and rename, so an interrupted or
        // concurrent download never leaves a truncated file behind.
        let tmp = dir.join(format!(".{f}.{}.part", std::process::id()));
        // Read the declared size before consuming the body (afterwards
        // reqwest reports the *remaining* length, i.e. 0).
        let expected_len = resp.content_length();
        let mut out = std::fs::File::create(&tmp)?;
        let n = resp
            .copy_to(&mut out)
            .with_context(|| format!("downloading {url}"))?;
        out.flush()?;
        if let Some(expected) = expected_len {
            if n != expected {
                let _ = std::fs::remove_file(&tmp);
                bail!("downloading {url}: got {n} of {expected} bytes");
            }
        }
        std::fs::rename(&tmp, &dest)?;
    }
    Ok(dir.to_string_lossy().into_owned())
}

fn with_model<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&StaticModel) -> R,
{
    let cell = MODEL.get_or_try_init(|| -> Result<Mutex<StaticModel>> {
        // normalize=None => honor the model config (potion normalizes).
        let location = resolve_model(&embed_model())?;
        let model = StaticModel::from_pretrained(&location, None, None, None)
            .with_context(|| format!("loading embedding model from {location}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_url_models_pass_through() {
        assert!(!is_url_model("minishlab/potion-retrieval-32M"));
        assert_eq!(
            resolve_model("minishlab/potion-retrieval-32M").unwrap(),
            "minishlab/potion-retrieval-32M"
        );
        assert_eq!(
            resolve_model("/opt/sndoc/model").unwrap(),
            "/opt/sndoc/model"
        );
    }

    #[test]
    fn url_models_get_a_stable_sanitised_cache_dir() {
        let a = url_model_dir("https://github.com/3n3a/sndoc/releases/download/embed-model-v1");
        let b = url_model_dir("https://github.com/3n3a/sndoc/releases/download/embed-model-v1/");
        assert_eq!(a, b);
        assert!(a.ends_with("github.com_3n3a_sndoc_releases_download_embed-model-v1"));
    }
}
