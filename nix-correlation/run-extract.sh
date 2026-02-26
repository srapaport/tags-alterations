#!/usr/bin/env bash
# run-extract.sh
# Robust wrapper that handles partial failures and large output

set -euo pipefail

NIXPKGS_REV="${1:-nixpkgs-unstable}"  # or pin a specific commit!
OUTPUT_DIR="./nix-sources-$(date +%Y%m%d)"
mkdir -p "$OUTPUT_DIR"

echo "[*] Extracting from nixpkgs channel: $NIXPKGS_REV"

# -------------------------------------------------------------------
# Option A: Use a pinned nixpkgs (RECOMMENDED for reproducibility)
# This ensures you can correlate your tag DB with a specific snapshot
# -------------------------------------------------------------------
NIX_PATH="nixpkgs=https://github.com/NixOS/nixpkgs/archive/${NIXPKGS_REV}.tar.gz"

nix-instantiate \
  --eval \
  --strict \
  --json \
  --show-trace \
  -I "$NIX_PATH" \
  extract-sources.nix \
  2>"$OUTPUT_DIR/eval-errors.log" \
| jq '.' > "$OUTPUT_DIR/raw-sources.json"

echo "[*] Post-processing..."

# Split into categories relevant to your research
jq '
  to_entries
  | map(select(.value.src.revType == "tag"))   # only tag-based refs!
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/tag-refs.json"

jq '
  to_entries
  | map(select(.value.src.revType == "commit"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/commit-refs.json"

jq '
  to_entries
  | map(select(.value.src.type == "fetchFromGitHub"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/github-refs.json"

# Summary stats
echo "[*] Summary:"
echo "  Total packages with sources: $(jq 'length' "$OUTPUT_DIR/raw-sources.json")"
echo "  Tag-based refs:              $(jq 'length' "$OUTPUT_DIR/tag-refs.json")"
echo "  Commit-based refs:           $(jq 'length' "$OUTPUT_DIR/commit-refs.json")"
echo "  GitHub sources:              $(jq 'length' "$OUTPUT_DIR/github-refs.json")"
echo "[*] Done → $OUTPUT_DIR/"
