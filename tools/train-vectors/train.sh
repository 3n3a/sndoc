#!/usr/bin/env bash
# Train sndoc's embedding model on the ServiceNow docs and (optionally) publish
# it as a GitHub release.
#
#   tools/train-vectors/train.sh [trainer args...]      # e.g. --exclude eval.tsv
#   PUBLISH_TAG=embed-model-v2 tools/train-vectors/train.sh
#
# Steps:
#   1. download the base model (minishlab/potion-retrieval-32M) into out/base
#   2. build the trainer and fine-tune into out/potion-sn-ft (gitignored)
#   3. if PUBLISH_TAG is set: upload the model files to that GitHub release of
#      this repo (created if missing, never marked "latest" so the CLI's
#      releases/latest download links keep pointing at the binaries)
#
# Needs the docs clone in the sndoc data dir (run `sndoc --no-index update`
# once; honours SNDOC_DATA_DIR). After training, try it locally with
#   SNDOC_EMBED_MODEL=$PWD/tools/train-vectors/out/potion-sn-ft sndoc index --force
# and update FINETUNED_EMBED_MODEL (src/core/constants.rs) when publishing a
# new release tag.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/out"
BASE="$OUT/base"
MODEL="$OUT/potion-sn-ft"
BASE_REPO="minishlab/potion-retrieval-32M"

mkdir -p "$BASE"
for f in config.json tokenizer.json model.safetensors tokenizer_config.json special_tokens_map.json vocab.txt; do
  if [ ! -s "$BASE/$f" ]; then
    echo "[train.sh] downloading $BASE_REPO/$f"
    curl -fSL --retry 3 "https://huggingface.co/$BASE_REPO/resolve/main/$f" -o "$BASE/$f.part"
    mv "$BASE/$f.part" "$BASE/$f"
  fi
done

(cd "$HERE" && cargo build --release --quiet)
"$HERE/target/release/train-vectors" --model-dir "$BASE" --out "$MODEL" "$@"
echo "[train.sh] model written to $MODEL"

if [ -n "${PUBLISH_TAG:-}" ]; then
  REPO_ROOT="$(cd "$HERE/../.." && pwd)"
  cd "$REPO_ROOT"
  if ! gh release view "$PUBLISH_TAG" >/dev/null 2>&1; then
    gh release create "$PUBLISH_TAG" --latest=false \
      --title "Embedding model $PUBLISH_TAG" \
      --notes "Fine-tuned static embeddings for sndoc (model2vec format, 512-dim), trained with tools/train-vectors from $BASE_REPO. Opt-in: SNDOC_EMBED_MODEL=https://github.com/$(gh repo view --json nameWithOwner -q .nameWithOwner)/releases/download/$PUBLISH_TAG. Not a CLI release."
  fi
  gh release upload "$PUBLISH_TAG" --clobber \
    "$MODEL/config.json" "$MODEL/tokenizer.json" "$MODEL/model.safetensors" "$MODEL/README.md"
  echo "[train.sh] published: https://github.com/$(gh repo view --json nameWithOwner -q .nameWithOwner)/releases/download/$PUBLISH_TAG"
fi
