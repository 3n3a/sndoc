//! Fine-tune sndoc's static embedding table (model2vec format) on the docs.
//!
//! The base model (`minishlab/potion-retrieval-32M`) is a token -> vector table;
//! a text's embedding is the L2-normalised mean of its token vectors. That is
//! differentiable by hand, so this trains the table directly with a contrastive
//! (InfoNCE, in-batch negatives) objective on pairs mined from the docs:
//!
//! * page title             -> the page's lead chunk
//! * page description       -> the page's lead chunk
//! * description minus the title words -> lead chunk (no name overlap)
//! * section heading        -> that section's chunk
//!
//! Positives use exactly the text sndoc indexes (`chunk_embed_text`) and the
//! same tokenisation as model2vec-rs (no special tokens, `[UNK]` dropped,
//! 512-token cap), so the output drops in via `SNDOC_EMBED_MODEL=<out dir>`.
//! Rows only move where the docs give a signal (tokens that never occur keep
//! their base vector); `--l2` optionally pulls touched rows back towards the
//! base table. Note the base rows are large (|x| ~ 4, row norm ~ 130: potion
//! bakes token weights into the norms), so the learning rate is on that scale
//! and per-token gradients are tiny; an L2 above ~1e-4 swamps the signal.
//! Copies of the same topic (same title) are masked out as negatives.
//!
//! Pages listed in `--exclude` (a TSV whose last column is a repo path, e.g.
//! the eval's `--dump-queries` output) are never trained on, so the eval stays
//! honest. A further 1% of pages is held out for the per-epoch validation.
//!
//! ```text
//! cargo run --release -- --model-dir <hf snapshot dir> --out <dir>
//!     [--data-dir <sndoc data dir>] [--exclude heldout.tsv]
//!     [--epochs 5] [--batch 512] [--lr 0.05] [--tau 0.05] [--l2 0] [--seed 1]
//! ```

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use sndoc::core::chunk::{chunk_doc, chunk_embed_text, parse_doc};
use sndoc::core::repo;
use sndoc::index::read_manifest;
use tokenizers::Tokenizer;

const MAX_TOKENS: usize = 512;
const MODEL_FILES: &[&str] = &[
    "config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "vocab.txt",
];

// ---------------------------------------------------------------- args ----

struct Args {
    model_dir: PathBuf,
    out: PathBuf,
    exclude: Option<PathBuf>,
    epochs: usize,
    batch: usize,
    lr: f32,
    tau: f32,
    l2: f32,
    seed: u64,
}

fn parse_args() -> Result<Args> {
    let a: Vec<String> = std::env::args().collect();
    let get = |n: &str| {
        a.iter()
            .position(|x| x == n)
            .and_then(|i| a.get(i + 1))
            .cloned()
    };
    if let Some(d) = get("--data-dir") {
        std::env::set_var("SNDOC_DATA_DIR", d);
    }
    let num = |n: &str, d: &str| get(n).unwrap_or_else(|| d.to_string());
    Ok(Args {
        model_dir: get("--model-dir")
            .context("--model-dir is required")?
            .into(),
        out: get("--out").context("--out is required")?.into(),
        exclude: get("--exclude").map(PathBuf::from),
        epochs: num("--epochs", "5").parse()?,
        batch: num("--batch", "512").parse()?,
        lr: num("--lr", "0.05").parse()?,
        tau: num("--tau", "0.05").parse()?,
        l2: num("--l2", "0").parse()?,
        seed: num("--seed", "1").parse()?,
    })
}

// ----------------------------------------------------------- safetensors ----

/// Read the `embeddings` F32 tensor of a model2vec safetensors file.
fn read_embeddings(path: &Path) -> Result<(Vec<f32>, usize, usize)> {
    let bytes = fs::read(path)?;
    let n = u64::from_le_bytes(bytes[..8].try_into()?) as usize;
    let header: serde_json::Value = serde_json::from_slice(&bytes[8..8 + n])?;
    let t = &header["embeddings"];
    if t["dtype"] != "F32" {
        bail!("expected F32 embeddings, got {}", t["dtype"]);
    }
    let rows = t["shape"][0].as_u64().context("shape")? as usize;
    let cols = t["shape"][1].as_u64().context("shape")? as usize;
    let start = 8 + n + t["data_offsets"][0].as_u64().context("offsets")? as usize;
    let data = bytes[start..start + rows * cols * 4]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    Ok((data, rows, cols))
}

fn write_embeddings(path: &Path, data: &[f32], rows: usize, cols: usize) -> Result<()> {
    let len = data.len() * 4;
    let mut header = serde_json::json!({
        "embeddings": {"dtype": "F32", "shape": [rows, cols], "data_offsets": [0, len]}
    })
    .to_string();
    while header.len() % 8 != 0 {
        header.push(' ');
    }
    let mut out = Vec::with_capacity(8 + header.len() + len);
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    for v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, out)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn save_model(args: &Args, table: &[f32], rows: usize, cols: usize, note: &str) -> Result<()> {
    fs::create_dir_all(&args.out)?;
    for f in MODEL_FILES {
        let src = args.model_dir.join(f);
        if src.exists() {
            fs::copy(&src, args.out.join(f))?;
        }
    }
    write_embeddings(&args.out.join("model.safetensors"), table, rows, cols)?;
    fs::write(
        args.out.join("README.md"),
        format!(
            "# sndoc fine-tuned static embeddings\n\nBase: `{}`\n\n{note}\n\n\
             Use with `SNDOC_EMBED_MODEL={}` (and reindex).\n",
            args.model_dir.display(),
            args.out.display()
        ),
    )?;
    Ok(())
}

// ------------------------------------------------------------------ data ----

/// A tokenised text as (token id, count) pairs plus total token count, which
/// is all mean pooling needs.
struct Bag {
    toks: Vec<(u32, f32)>,
    n: f32,
}

fn bag(ids: &[u32], unk: Option<u32>) -> Bag {
    let mut m: HashMap<u32, f32> = HashMap::new();
    let mut n = 0usize;
    for &id in ids.iter().filter(|&&id| Some(id) != unk).take(MAX_TOKENS) {
        *m.entry(id).or_default() += 1.0;
        n += 1;
    }
    let mut toks: Vec<(u32, f32)> = m.into_iter().collect();
    toks.sort_unstable_by_key(|t| t.0);
    Bag {
        toks,
        n: n.max(1) as f32,
    }
}

struct Pair {
    query: usize, // index into texts
    doc: usize,   // index into texts
    group: u32,   // topic id (same title => same group)
}

struct Data {
    texts: Vec<Bag>,
    train: Vec<Pair>,
    val: Vec<Pair>,
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

fn build_data(args: &Args, tok: &Tokenizer, unk: Option<u32>) -> Result<Data> {
    let manifest = read_manifest().context("no sndoc index/manifest in the data dir")?;
    let exclude: HashSet<String> = match &args.exclude {
        Some(p) => fs::read_to_string(p)?
            .lines()
            .filter_map(|l| l.rsplit('\t').next())
            .map(|s| s.trim().to_string())
            .collect(),
        None => HashSet::new(),
    };
    let docs = repo::read_all_markdown(&manifest.branch)?;
    eprintln!(
        "[train] {} pages on {}; excluding {} eval pages",
        docs.len(),
        manifest.branch,
        exclude.len()
    );

    let mut raw: Vec<String> = Vec::new();
    let mut text_id: HashMap<String, usize> = HashMap::new();
    let mut intern = |s: String, raw: &mut Vec<String>| -> usize {
        *text_id.entry(s.clone()).or_insert_with(|| {
            raw.push(s);
            raw.len() - 1
        })
    };
    let mut groups: HashMap<String, u32> = HashMap::new();
    let (mut train, mut val) = (Vec::new(), Vec::new());
    let mut rng = Rng(args.seed ^ 0x9e37_79b9);
    for (path, md) in &docs {
        if exclude.contains(path) {
            continue;
        }
        let meta = parse_doc(md).meta;
        let chunks = chunk_doc(md, path, &manifest.branch);
        let Some(lead) = chunks.first() else { continue };
        let ng = groups.len() as u32;
        let group = *groups.entry(meta.title.to_lowercase()).or_insert(ng);
        let to_val = rng.next() < 0.01;
        let lead_id = intern(chunk_embed_text(lead), &mut raw);
        let mut pairs: Vec<(String, usize)> = Vec::new();
        if !meta.title.is_empty() {
            pairs.push((meta.title.clone(), lead_id));
        }
        if meta.description.len() > 20 {
            pairs.push((meta.description.clone(), lead_id));
            let tw: HashSet<String> = words(&meta.title).into_iter().collect();
            let rest: Vec<String> = words(&meta.description)
                .into_iter()
                .filter(|w| !tw.contains(w))
                .collect();
            if rest.len() >= 3 {
                pairs.push((rest.join(" "), lead_id));
            }
        }
        for c in chunks.iter().skip(1) {
            if !c.heading.is_empty() && c.heading != meta.title && c.content.len() > 80 {
                let d = intern(chunk_embed_text(c), &mut raw);
                pairs.push((format!("{} {}", meta.title, c.heading), d));
            }
        }
        for (q, d) in pairs {
            let p = Pair {
                query: intern(q, &mut raw),
                doc: d,
                group,
            };
            if to_val {
                val.push(p)
            } else {
                train.push(p)
            }
        }
    }
    eprintln!(
        "[train] {} train pairs, {} val pairs, {} unique texts; tokenising...",
        train.len(),
        val.len(),
        raw.len()
    );
    let texts: Vec<Bag> = raw
        .par_chunks(2048)
        .flat_map_iter(|batch| {
            let enc = tok
                .encode_batch_fast(batch.to_vec(), false)
                .expect("tokenise");
            enc.into_iter()
                .map(|e| bag(e.get_ids(), unk))
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(Data { texts, train, val })
}

// ----------------------------------------------------------------- model ----

struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn next(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Mean-pool + L2-normalise. Returns (unit vector, norm of the mean).
fn embed(table: &[f32], dim: usize, b: &Bag) -> (Vec<f32>, f32) {
    let mut s = vec![0f32; dim];
    for &(t, c) in &b.toks {
        let row = &table[t as usize * dim..(t as usize + 1) * dim];
        for (x, &v) in s.iter_mut().zip(row) {
            *x += c * v;
        }
    }
    for x in &mut s {
        *x /= b.n;
    }
    let norm = s.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
    for x in &mut s {
        *x /= norm;
    }
    (s, norm)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Retrieval accuracy@1 of val queries against all val positives (plus
/// in-group copies counted as correct).
fn validate(table: &[f32], dim: usize, data: &Data) -> (f64, f64) {
    let docs: Vec<usize> = {
        let mut d: Vec<usize> = data.val.iter().map(|p| p.doc).collect();
        d.sort_unstable();
        d.dedup();
        d
    };
    let dv: Vec<Vec<f32>> = docs
        .par_iter()
        .map(|&d| embed(table, dim, &data.texts[d]).0)
        .collect();
    let doc_group: HashMap<usize, u32> = data.val.iter().map(|p| (p.doc, p.group)).collect();
    let (hits, rr): (Vec<f64>, Vec<f64>) = data
        .val
        .par_iter()
        .map(|p| {
            let q = embed(table, dim, &data.texts[p.query]).0;
            let target = dot(&q, &dv[docs.binary_search(&p.doc).unwrap()]);
            // rank = 1 + docs of other groups scoring higher
            let better = docs
                .iter()
                .zip(&dv)
                .filter(|(d, v)| doc_group[d] != p.group && dot(&q, v) > target)
                .count();
            ((better == 0) as u8 as f64, 1.0 / (better + 1) as f64)
        })
        .unzip();
    let n = hits.len().max(1) as f64;
    (hits.iter().sum::<f64>() / n, rr.iter().sum::<f64>() / n)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let tok = Tokenizer::from_file(args.model_dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("loading tokenizer: {e}"))?;
    let unk = tok.token_to_id("[UNK]");
    let (mut table, rows, dim) = read_embeddings(&args.model_dir.join("model.safetensors"))?;
    let base = table.clone();
    eprintln!("[train] base table {rows} x {dim}");

    let data = build_data(&args, &tok, unk)?;
    let (acc, mrr) = validate(&table, dim, &data);
    eprintln!("[train] epoch 0 (base): val acc@1 {acc:.4}  MRR {mrr:.4}");

    // Lazy Adam over touched rows only.
    let (b1, b2, eps) = (0.9f32, 0.999f32, 1e-8f32);
    let mut m = vec![0f32; table.len()];
    let mut v = vec![0f32; table.len()];
    let mut step = 0i32;
    let mut order: Vec<usize> = (0..data.train.len()).collect();
    let mut rng = Rng(args.seed);
    let mut best = mrr;

    for epoch in 1..=args.epochs {
        let t0 = Instant::now();
        for i in (1..order.len()).rev() {
            order.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
        }
        let mut loss_sum = 0f64;
        let mut batches = 0usize;
        for batch in order.chunks(args.batch) {
            if batch.len() < 8 {
                continue;
            }
            let pairs: Vec<&Pair> = batch.iter().map(|&i| &data.train[i]).collect();
            let b = pairs.len();
            let q: Vec<(Vec<f32>, f32)> = pairs
                .par_iter()
                .map(|p| embed(&table, dim, &data.texts[p.query]))
                .collect();
            let d: Vec<(Vec<f32>, f32)> = pairs
                .par_iter()
                .map(|p| embed(&table, dim, &data.texts[p.doc]))
                .collect();

            // Logits S[i][j] = q_i . d_j / tau, other copies of the topic masked.
            let s: Vec<Vec<f32>> = (0..b)
                .into_par_iter()
                .map(|i| {
                    (0..b)
                        .map(|j| {
                            if i != j && pairs[i].group == pairs[j].group {
                                f32::NEG_INFINITY
                            } else {
                                dot(&q[i].0, &d[j].0) / args.tau
                            }
                        })
                        .collect()
                })
                .collect();
            // Symmetric InfoNCE: G = (softmax_rows - I + softmax_cols - I) / (2b).
            let mut g = vec![vec![0f32; b]; b];
            let mut loss = 0f64;
            for i in 0..b {
                let mx = s[i].iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let z: f32 = s[i].iter().map(|x| (x - mx).exp()).sum();
                loss -= ((s[i][i] - mx).exp() / z).ln() as f64;
                for j in 0..b {
                    g[i][j] += ((s[i][j] - mx).exp() / z - (i == j) as u8 as f32) / (2 * b) as f32;
                }
            }
            for j in 0..b {
                let mx = (0..b).map(|i| s[i][j]).fold(f32::NEG_INFINITY, f32::max);
                let z: f32 = (0..b).map(|i| (s[i][j] - mx).exp()).sum();
                loss -= ((s[j][j] - mx).exp() / z).ln() as f64;
                for i in 0..b {
                    g[i][j] += ((s[i][j] - mx).exp() / z - (i == j) as u8 as f32) / (2 * b) as f32;
                }
            }
            loss_sum += loss / (2 * b) as f64;
            batches += 1;

            // dL/du for every text in the batch, then back through the
            // normalisation and the mean onto token rows.
            let du_q: Vec<Vec<f32>> = (0..b)
                .into_par_iter()
                .map(|i| {
                    let mut out = vec![0f32; dim];
                    for j in 0..b {
                        let c = g[i][j] / args.tau;
                        if c != 0.0 {
                            out.iter_mut().zip(&d[j].0).for_each(|(o, x)| *o += c * x);
                        }
                    }
                    out
                })
                .collect();
            let du_d: Vec<Vec<f32>> = (0..b)
                .into_par_iter()
                .map(|j| {
                    let mut out = vec![0f32; dim];
                    for i in 0..b {
                        let c = g[i][j] / args.tau;
                        if c != 0.0 {
                            out.iter_mut().zip(&q[i].0).for_each(|(o, x)| *o += c * x);
                        }
                    }
                    out
                })
                .collect();
            let mut grads: HashMap<u32, Vec<f32>> = HashMap::new();
            let items = pairs
                .iter()
                .zip(q.iter().zip(&du_q))
                .map(|(p, (e, du))| (p.query, e, du))
                .chain(
                    pairs
                        .iter()
                        .zip(d.iter().zip(&du_d))
                        .map(|(p, (e, du))| (p.doc, e, du)),
                );
            for (text, (u, norm), du) in items {
                let proj = dot(u, du);
                let ds: Vec<f32> = du
                    .iter()
                    .zip(u)
                    .map(|(g, u)| (g - u * proj) / norm)
                    .collect();
                let bagt = &data.texts[text];
                for &(t, c) in &bagt.toks {
                    let w = c / bagt.n;
                    let row = grads.entry(t).or_insert_with(|| vec![0f32; dim]);
                    row.iter_mut().zip(&ds).for_each(|(r, x)| *r += w * x);
                }
            }

            step += 1;
            let (c1, c2) = (1.0 - b1.powi(step), 1.0 - b2.powi(step));
            let lr = args.lr * c2.sqrt() / c1;
            let l2 = args.l2;
            // Rows are disjoint, so update them in parallel.
            let mut touched: Vec<(u32, Vec<f32>)> = grads.into_iter().collect();
            touched.sort_unstable_by_key(|t| t.0);
            let tbl = SyncPtr(table.as_mut_ptr());
            let mp = SyncPtr(m.as_mut_ptr());
            let vp = SyncPtr(v.as_mut_ptr());
            touched.par_iter().for_each(|(t, gr)| {
                let off = *t as usize * dim;
                // SAFETY: each token id appears once in `touched`, so rows are
                // disjoint across threads.
                unsafe {
                    for k in 0..dim {
                        let w = tbl.get().add(off + k);
                        let mk = mp.get().add(off + k);
                        let vk = vp.get().add(off + k);
                        let grad = gr[k] + l2 * (*w - base[off + k]);
                        *mk = b1 * *mk + (1.0 - b1) * grad;
                        *vk = b2 * *vk + (1.0 - b2) * grad * grad;
                        *w -= lr * *mk / ((*vk).sqrt() + eps);
                    }
                }
            });
            if batches % 100 == 0 {
                eprintln!(
                    "[train]   epoch {epoch} batch {batches}/{}  loss {:.4}",
                    order.len() / args.batch,
                    loss_sum / batches as f64
                );
            }
        }
        let (acc, mrr) = validate(&table, dim, &data);
        eprintln!(
            "[train] epoch {epoch}: loss {:.4}  val acc@1 {acc:.4}  MRR {mrr:.4}  ({:.0}s)",
            loss_sum / batches.max(1) as f64,
            t0.elapsed().as_secs_f64()
        );
        if mrr > best {
            best = mrr;
            save_model(
                &args,
                &table,
                rows,
                dim,
                &format!("Epoch {epoch}: val acc@1 {acc:.4}, MRR {mrr:.4}."),
            )?;
            eprintln!("[train]   saved -> {}", args.out.display());
        }
    }
    eprintln!("[train] done; best val MRR {best:.4}");
    Ok(())
}

#[derive(Clone, Copy)]
struct SyncPtr(*mut f32);
unsafe impl Send for SyncPtr {}
unsafe impl Sync for SyncPtr {}
impl SyncPtr {
    fn get(self) -> *mut f32 {
        self.0
    }
}
