# train-vectors

Fine-tunes the static embedding table sndoc uses for its vector search arm on
the ServiceNow docs. A developer utility: it is a separate crate and not part of
the `sndoc` binary.

## Why

sndoc's vector arm uses [model2vec](https://github.com/MinishLab/model2vec)
static embeddings: a token → vector table, mean-pooled and L2-normalised. The
base model (`minishlab/potion-retrieval-32M`) is general-purpose; on these docs
its vector arm alone ranked the right topic first for only about a quarter of
the hardest eval queries. Because the embedding is just a mean of table rows,
the table can be trained directly, without a transformer or GPU.

## How it works

- **Training pairs, mined from the docs** (≈370k): page title → the page's
  lead chunk, page description → lead chunk, description without the title's
  words → lead chunk, and "title + section heading" → that section's chunk.
  The chunk side is exactly the text sndoc embeds (`chunk_embed_text`).
- **Objective:** symmetric InfoNCE with in-batch negatives (batch 512,
  temperature 0.05). Copies of the same topic published under several paths
  (same title) are masked out as negatives.
- **Gradients by hand** through mean pooling and the L2 normalisation, then
  lazy Adam on the touched rows only (lr 0.05). Tokens that never occur in the
  docs keep their base vectors.
- **Tokenisation** matches model2vec-rs: no special tokens, `[UNK]` dropped,
  512-token cap.
- **Validation:** 1% of pages are held out, and the best epoch by validation
  MRR is saved. `--exclude <tsv>` removes further pages, such as the eval
  sets, so the eval stays honest.

It takes about 3 minutes on a laptop CPU for 5 epochs.

## Usage

```bash
sndoc --no-index update            # once: the docs clone the trainer reads
tools/train-vectors/train.sh       # -> tools/train-vectors/out/potion-sn-ft (gitignored)

# try it locally (the index is rebuilt automatically for a new model)
SNDOC_EMBED_MODEL=$PWD/tools/train-vectors/out/potion-sn-ft sndoc index --force

# publish as a GitHub release (never marked "latest"), then point
# DEFAULT_EMBED_MODEL in src/core/constants.rs at the new tag
PUBLISH_TAG=embed-model-v2 tools/train-vectors/train.sh
```

`train.sh` passes extra arguments through to the trainer (`--epochs`, `--lr`,
`--batch`, `--tau`, `--l2`, `--seed`, `--exclude`). `out/base` caches the
downloaded base model.

## Results

Measured with `examples/eval.rs` (MRR; R@10 in brackets) on two sets:

- **Synthetic, held out:** 300 random topics queried by their title, their
  description, and their description without the title's words. The model
  used here was trained *without* these pages (and without a separate tuning
  sample).
- **Real questions:** the 27 searches from actual sndoc sessions in
  `examples/eval-questions.tsv`, each with the page(s) that answer it.

| Setup | title | description | desc. minus title | real questions |
|---|---|---|---|---|
| v0.5 ranking, base model | 0.700 | 0.840 | 0.583 | 0.487 (0.82) |
| v0.6 ranking, base model | 0.804 | 0.910 | 0.775 | 0.483 (0.85) |
| **v0.6 ranking, fine-tuned model** | **0.873** | **0.959** | **0.814** | **0.505 (0.89)** |
| vector arm alone: base → fine-tuned | 0.619 → 0.730 | 0.767 → 0.887 | 0.316 → 0.562 | |

The published model (`embed-model-v1`) is the same recipe trained on all
pages, which is what the real-question column uses. The vector weight
(`w_vec = 0.3`) was re-tuned for it on a separate sample: higher weights lose
on the synthetic sets and gain at most one question on the real set.
