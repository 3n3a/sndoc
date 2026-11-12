//! Retrieval-quality eval for `sndoc search` (known-item retrieval).
//!
//! Samples topics from the indexed branch of the local clone and turns each
//! into queries whose correct answer is that topic:
//!
//! * `title`: the frontmatter title (exact-name lookups)
//! * `desc`: the frontmatter description (a one-line summary)
//! * `desc-title`: the description minus any word of the title, so the query
//!   cannot match on the topic's name
//!
//! A hit counts if it is the topic itself or a copy published under another
//! path (same title). Reports R@1/R@3/R@10 and MRR per query type for the
//! v0.5 ranking (`legacy`) and the current default, or a parameter sweep.
//!
//! Queries are derived from the docs, so this measures known-item lookup, not
//! open-ended questions; use `--queries` for a hand-written set.
//!
//! ```text
//! cargo run --release --example eval -- [--sample 300] [--seed 42] [--sweep] [--sweep-rn]
//!                                        [--sweep-wvec] [--verbose]  # per-query ranks
//!                                        [--queries file.tsv]   # [kind<TAB>]query<TAB>path
//!                                        [--dump-queries file.tsv]  # write the sampled set
//!                                        [--dump-only]              # ... and exit
//! ```
//! Uses the index in the data dir (`SNDOC_DATA_DIR` / `--data-dir`); run
//! `sndoc index` first.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use sndoc::core::chunk::parse_doc;
use sndoc::core::constants::index_db_path;
use sndoc::core::embed::embed_query;
use sndoc::core::index_store::{is_release_notes_path, Candidate, IndexStore, SearchParams};
use sndoc::core::repo;
use sndoc::index::read_manifest;

const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "for", "with", "on", "by", "is", "are", "be",
    "as", "it", "this", "that", "from", "at", "use", "using", "your", "you", "can", "how", "when",
    "which", "into", "not",
];

const MAX_POOL: usize = 400;

struct Topic {
    path: String,
    title: String,
    description: String,
}

struct Query {
    kind: &'static str,
    text: String,
    target: usize, // index into topics
    /// Further acceptable answers (a `--queries` line repeated with another path).
    alts: Vec<usize>,
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .replace('\\', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn words(s: &str) -> Vec<String> {
    norm(s)
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Small deterministic LCG so samples are reproducible without a rand crate.
struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 as f64 / 4_294_967_296.0
    }
}

#[derive(Default, Clone)]
struct Metrics {
    n: usize,
    r1: usize,
    r3: usize,
    r10: usize,
    rr: f64,
}

impl Metrics {
    fn add(&mut self, rank: Option<usize>) {
        self.n += 1;
        if let Some(r) = rank {
            self.r1 += (r <= 1) as usize;
            self.r3 += (r <= 3) as usize;
            self.r10 += (r <= 10) as usize;
            self.rr += 1.0 / r as f64;
        }
    }
    fn mrr(&self) -> f64 {
        self.rr / self.n.max(1) as f64
    }
    fn row(&self) -> String {
        let f = |x: usize| x as f64 / self.n.max(1) as f64;
        format!(
            "n={:<4} R@1 {:.3}  R@3 {:.3}  R@10 {:.3}  MRR {:.3}",
            self.n,
            f(self.r1),
            f(self.r3),
            f(self.r10),
            self.mrr()
        )
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(dir) = arg(&args, "--data-dir") {
        std::env::set_var("SNDOC_DATA_DIR", dir);
    }
    let sample: usize = arg(&args, "--sample").map_or(300, |s| s.parse().unwrap());
    let seed: u32 = arg(&args, "--seed").map_or(42, |s| s.parse().unwrap());
    let sweep = args.iter().any(|a| a == "--sweep");
    let verbose = args.iter().any(|a| a == "--verbose");

    let Some(manifest) = read_manifest() else {
        bail!("no index found; run `sndoc index` first");
    };
    eprintln!(
        "[eval] index: {} @ {} (schema {}), {} chunks",
        manifest.branch,
        &manifest.commit[..8],
        manifest.schema_version,
        manifest.chunk_count
    );

    // Topics from the same branch the index was built from.
    let mut topics: Vec<Topic> = Vec::new();
    for (path, md) in repo::read_all_markdown(&manifest.branch)? {
        let meta = parse_doc(&md).meta;
        topics.push(Topic {
            path,
            title: meta.title,
            description: meta.description,
        });
    }

    let mut queries: Vec<Query> = Vec::new();
    if let Some(file) = arg(&args, "--queries") {
        let by_path: HashMap<&str, usize> = topics
            .iter()
            .enumerate()
            .map(|(i, t)| (t.path.as_str(), i))
            .collect();
        for line in std::fs::read_to_string(file)?.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            let (kind, q, p) = match cols.as_slice() {
                [k, q, p] => (*k, *q, *p),
                [q, p] => ("manual", *q, *p),
                _ => continue,
            };
            let kind: &'static str = Box::leak(kind.to_string().into_boxed_str());
            match by_path.get(p.trim()) {
                // Consecutive lines with the same query = several right answers.
                Some(&i) => match queries.last_mut() {
                    Some(last) if last.kind == kind && last.text == q => last.alts.push(i),
                    _ => queries.push(Query {
                        kind,
                        text: q.to_string(),
                        target: i,
                        alts: Vec::new(),
                    }),
                },
                None => eprintln!("[eval] unknown path, skipped: {p}"),
            }
        }
    } else {
        let pool: Vec<usize> = (0..topics.len())
            .filter(|&i| !topics[i].title.is_empty() && topics[i].description.len() > 40)
            .collect();
        let mut rng = Lcg(seed);
        let mut used = HashSet::new();
        while used.len() < sample.min(pool.len()) {
            let i = pool[(rng.next() * pool.len() as f64) as usize];
            if !used.insert(i) {
                continue;
            }
            let t = &topics[i];
            queries.push(Query {
                kind: "title",
                text: t.title.clone(),
                target: i,
                alts: Vec::new(),
            });
            queries.push(Query {
                kind: "desc",
                text: t.description.clone(),
                target: i,
                alts: Vec::new(),
            });
            let tw: HashSet<String> = words(&t.title).into_iter().collect();
            let rest: Vec<String> = words(&t.description)
                .into_iter()
                .filter(|w| !tw.contains(w) && !STOP.contains(&w.as_str()))
                .collect();
            if !rest.is_empty() {
                queries.push(Query {
                    kind: "desc-title",
                    text: rest.join(" "),
                    target: i,
                    alts: Vec::new(),
                });
            }
        }
    }

    if let Some(file) = arg(&args, "--dump-queries") {
        let lines: Vec<String> = queries
            .iter()
            .map(|q| {
                format!(
                    "{}\t{}\t{}",
                    q.kind,
                    q.text.replace(['\t', '\n'], " "),
                    topics[q.target].path
                )
            })
            .collect();
        std::fs::write(&file, lines.join("\n") + "\n")?;
        eprintln!("[eval] wrote {} queries to {file}", lines.len());
    }

    if args.iter().any(|a| a == "--dump-only") {
        return Ok(());
    }

    // Embed every query once; reuse across configurations.
    let vecs: Vec<Vec<f32>> = queries
        .iter()
        .map(|q| embed_query(&q.text))
        .collect::<Result<_>>()?;
    let store = IndexStore::open(&index_db_path().to_string_lossy())?;
    // The vector arm doesn't depend on any tunable except the pool size (KNN
    // top-k is a prefix of a larger top-k), so compute it once per query.
    let t0 = std::time::Instant::now();
    let vec_arms: Vec<Vec<Candidate>> = vecs
        .iter()
        .map(|v| store.vector_arm(v, MAX_POOL))
        .collect::<Result<_>>()?;
    eprintln!(
        "[eval] vector arm: {:.1} ms/query",
        t0.elapsed().as_secs_f64() * 1000.0 / queries.len().max(1) as f64
    );

    let mut configs: Vec<(String, SearchParams)> = vec![
        ("legacy".into(), SearchParams::legacy()),
        ("default".into(), SearchParams::default()),
        (
            "bm25-only".into(),
            SearchParams {
                w_vec: 0.0,
                ..SearchParams::default()
            },
        ),
        (
            "vector-only".into(),
            SearchParams {
                w_fts: 0.0,
                ..SearchParams::default()
            },
        ),
    ];
    if args.iter().any(|a| a == "--sweep-wvec") {
        for w_vec in [0.2, 0.3, 0.5, 0.7, 1.0, 1.5, 2.0] {
            configs.push((
                format!("w_vec={w_vec}"),
                SearchParams {
                    w_vec,
                    ..SearchParams::default()
                },
            ));
        }
    }
    if args.iter().any(|a| a == "--sweep-rn") {
        for w in [1.0, 0.8, 0.6, 0.5, 0.4, 0.3] {
            configs.push((
                format!("release_notes_weight={w}"),
                SearchParams {
                    release_notes_weight: w,
                    ..SearchParams::default()
                },
            ));
        }
    }
    if sweep {
        let base = SearchParams::default();
        for w_vec in [0.2, 0.3, 0.4, 0.5, 0.7] {
            for cw in [
                [1.0, 5.0, 1.0, 3.0],
                [1.0, 10.0, 2.0, 5.0],
                [1.0, 10.0, 2.0, 10.0],
                [1.0, 20.0, 2.0, 10.0],
                [1.0, 20.0, 4.0, 20.0],
            ] {
                for pool in [40, 200] {
                    let p = SearchParams {
                        w_vec,
                        column_weights: cw,
                        pool,
                        ..base.clone()
                    };
                    configs.push((format!("w_vec={w_vec} cols={cw:?} pool={pool}"), p));
                }
            }
        }
        for doc_level in [false, true] {
            configs.push((
                format!("default doc={doc_level}"),
                SearchParams {
                    doc_level,
                    ..base.clone()
                },
            ));
        }
        for collapse_duplicates in [false, true] {
            configs.push((
                format!("default collapse={collapse_duplicates}"),
                SearchParams {
                    collapse_duplicates,
                    ..base.clone()
                },
            ));
        }
        for drop_stopwords in [false, true] {
            configs.push((
                format!("default stop={drop_stopwords}"),
                SearchParams {
                    drop_stopwords,
                    ..base.clone()
                },
            ));
        }
    }

    let kinds: Vec<&str> = {
        let mut k: Vec<&str> = queries.iter().map(|q| q.kind).collect();
        k.dedup();
        let mut seen = HashSet::new();
        k.retain(|x| seen.insert(*x));
        k
    };
    let mut summary: Vec<(String, f64)> = Vec::new();
    let mut fts_cache: HashMap<String, Vec<Vec<Candidate>>> = HashMap::new();
    for (name, params) in &configs {
        let t0 = std::time::Instant::now();
        let mut per: HashMap<&str, Metrics> = HashMap::new();
        // Share of top-10 slots taken by release notes, over queries whose
        // target is not itself a release-notes page.
        let (mut rn_slots, mut all_slots) = (0usize, 0usize);
        // The BM25 arm depends only on column weights + stopwords (top-k is a
        // prefix of a larger top-k), so cache it across the other knobs.
        let fts_key = format!("{:?}/{}", params.column_weights, params.drop_stopwords);
        if !fts_cache.contains_key(&fts_key) {
            let arms = queries
                .iter()
                .map(|q| store.fts_arm(&q.text, MAX_POOL, params))
                .collect::<Result<Vec<_>>>()?;
            fts_cache.insert(fts_key.clone(), arms);
        }
        let fts_arms = &fts_cache[&fts_key];
        for (qi, q) in queries.iter().enumerate() {
            let target = &topics[q.target];
            let pool = params.pool.max(40);
            let fts = &fts_arms[qi][..fts_arms[qi].len().min(pool)];
            let vec_arm = &vec_arms[qi][..vec_arms[qi].len().min(pool)];
            let hits = store.fuse(&q.text, fts, vec_arm, 10, params)?;
            let answers: Vec<&Topic> = std::iter::once(q.target)
                .chain(q.alts.iter().copied())
                .map(|i| &topics[i])
                .collect();
            let rank = hits
                .iter()
                .position(|h| {
                    answers
                        .iter()
                        .any(|t| h.path == t.path || norm(&h.title) == norm(&t.title))
                })
                .map(|i| i + 1);
            per.entry(q.kind).or_default().add(rank);
            if verbose {
                let r = rank.map_or("  -".to_string(), |r| format!("{r:>3}"));
                let top = hits.first().map_or("", |h| h.path.as_str());
                println!("   {r}  {}   [top: {top}]", q.text);
            }
            if !is_release_notes_path(&target.path) {
                rn_slots += hits
                    .iter()
                    .filter(|h| is_release_notes_path(&h.path))
                    .count();
                all_slots += hits.len();
            }
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / queries.len() as f64;
        println!(
            "== {name}  ({ms:.1} ms/query incl. any uncached BM25; release notes in top-10: {:.1}%)",
            100.0 * rn_slots as f64 / all_slots.max(1) as f64
        );
        let mut mean = 0.0;
        for k in &kinds {
            let m = &per[k];
            println!("   {k:<11} {}", m.row());
            mean += m.mrr();
        }
        summary.push((name.clone(), mean / kinds.len() as f64));
    }
    if sweep {
        summary.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        println!("\n== mean MRR across query types (best first)");
        for (name, m) in summary.iter().take(12) {
            println!("   {m:.3}  {name}");
        }
    }
    Ok(())
}
