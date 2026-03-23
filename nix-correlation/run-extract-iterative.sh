#!/usr/bin/env bash
# run-extract-iterative.sh
# Iteratively extract sources, adding problematic packages to blacklist

set -euo pipefail
#./run-extact-iterative.sh 08ffd21f7c8f90e15216bc329855b66ffea8526d
NIXPKGS_REV="${1:-nixpkgs-unstable}"
OUTPUT_DIR="./nix-sources-$(date +%Y%m%d)"
mkdir -p "$OUTPUT_DIR"

MAX_ITERATIONS=50  # Prevent infinite loops
iteration=0

echo "[*] Extracting from nixpkgs channel: $NIXPKGS_REV"
echo "[*] Using iterative approach to handle platform-specific packages"

NIX_PATH="nixpkgs=https://github.com/NixOS/nixpkgs/archive/${NIXPKGS_REV}.tar.gz"

while [ $iteration -lt $MAX_ITERATIONS ]; do
  iteration=$((iteration + 1))
  echo "[*] Iteration $iteration..."

  if nix-instantiate \
    --eval \
    --strict \
    --json \
    --show-trace \
    -I "$NIX_PATH" \
    extract-sources.nix \
    2>"$OUTPUT_DIR/eval-errors.log" \
    > "$OUTPUT_DIR/raw-sources.json"; then

    echo "[+] Extraction successful!"
    break
  else
    # FIX: use head -1 on the error line to get root cause, not deepest frame
    failed_pkg=$(grep "^error:" "$OUTPUT_DIR/eval-errors.log" \
      | grep -oE "'[a-zA-Z0-9_-]+'" \
      | head -1 \
      | tr -d "'" \
      || echo "")

    if [ -z "$failed_pkg" ]; then
      # Pattern 1: /pkgs/by-name/xx/packagename/package.nix
      failed_pkg=$(grep -E "^\s+at .*/pkgs/by-name/[^/]+/[^/]+/package.nix" "$OUTPUT_DIR/eval-errors.log" \
        | head -1 \
        | sed -E 's#.*/pkgs/by-name/[^/]+/([^/]+)/package.nix.*#\1#' \
        || echo "")
    fi

    if [ -z "$failed_pkg" ]; then
      # Pattern 2: /pkgs/development/*/packagename/default.nix
      failed_pkg=$(grep -E "^\s+at .*/pkgs/[^/]+/[^/]+/[^/]+/(default|package).nix" "$OUTPUT_DIR/eval-errors.log" \
        | head -1 \
        | sed -E 's#.*/pkgs/[^/]+/[^/]+/([^/]+)/(default|package).nix.*#\1#' \
        || echo "")
    fi

    if [ -z "$failed_pkg" ]; then
      # Pattern 3: any package path
      failed_pkg=$(grep -oE "/pkgs/[^/]+/[^/]+/[^/]+" "$OUTPUT_DIR/eval-errors.log" \
        | grep -v "pkgs/build-support\|pkgs/stdenv" \
        | head -1 \
        | awk -F'/' '{print $(NF)}' \
        || echo "")
    fi

    if [ -z "$failed_pkg" ]; then
      echo "[!] Could not identify failing package. Check $OUTPUT_DIR/eval-errors.log"
      echo "[!] Last error:"
      tail -30 "$OUTPUT_DIR/eval-errors.log"
      if [ -s "$OUTPUT_DIR/raw-sources.json" ]; then
        echo "[*] Found some results, using partial extraction"
        break
      fi
      exit 1
    fi

    # Skip if it's not a real package name
    if [[ "$failed_pkg" == *"-modules" ]] || [[ "$failed_pkg" == "lib" ]] || [[ "$failed_pkg" == "pkgs" ]]; then
      echo "[!] '$failed_pkg' is not a package name, likely a path component"
      if [ -s "$OUTPUT_DIR/raw-sources.json" ]; then
        echo "[*] Found partial results, stopping here"
        break
      fi
      echo "[!] No results yet, cannot continue"
      exit 1
    fi

    echo "[!] Package '$failed_pkg' failed evaluation, adding to blacklist..."

    if grep -q "\"$failed_pkg\"" extract-sources.nix; then
      echo "[!] Package already in blacklist but still failing. Check extract-sources.nix"
      exit 1
    fi

    # macOS-compatible sed
    sed -i '' '/# Add more as discovered/a\
    "'"$failed_pkg"'"     # auto-added by iterative extraction\
' extract-sources.nix

    echo "[+] Added '$failed_pkg' to blacklist, retrying..."
  fi
done

if [ $iteration -ge $MAX_ITERATIONS ]; then
  echo "[!] Reached maximum iterations ($MAX_ITERATIONS). Too many failing packages."
  exit 1
fi

# ------------------------------------------------------------------ #
# Post-processing
# FIX: all filters now use rev_type (snake_case) matching the Nix
#      output field names defined in extract-sources.nix
# ------------------------------------------------------------------ #
echo "[*] Post-processing..."

# Tag references — the core dataset for supply-chain research
jq '
  to_entries
  | map(select(.value.src.rev_type == "tag"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/tag-refs.json"

# Commit references (all commit variants: sha1, sha1-abbrev, sha256)
jq '
  to_entries
  | map(select(.value.src.rev_type | startswith("commit")))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/commit-refs.json"

# SVN refs (numeric revisions — not mutable in the same way but worth tracking)
jq '
  to_entries
  | map(select(.value.src.rev_type == "svn-revnum"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/svn-refs.json"

# No VCS reference at all (fetchurl, fetchzip, fetchTarball)
jq '
  to_entries
  | map(select(.value.src.rev_type == "none"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/no-rev-refs.json"

# Per-forge breakdown
jq '
  to_entries
  | map(select(.value.src.type == "fetchFromGitHub"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/github-refs.json"

jq '
  to_entries
  | map(select(.value.src.type == "fetchFromGitLab"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/gitlab-refs.json"

jq '
  to_entries
  | map(select(.value.src.type == "fetchFromBitbucket"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/bitbucket-refs.json"

jq '
  to_entries
  | map(select(.value.src.type == "fetchFromSourcehut"))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/sourcehut-refs.json"

jq '
  to_entries
  | map(select(.value.src.type | IN("fetchFromForgejo","fetchFromGitea","fetchFromGogs")))
  | from_entries
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/other-forge-refs.json"

# Cross-cut: tag references grouped by forge type — most relevant for the paper
jq '
  to_entries
  | map(select(.value.src.rev_type == "tag"))
  | group_by(.value.src.type)
  | map({ forge: .[0].value.src.type, count: length })
  | sort_by(-.count)
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/tags-by-forge.json"

# Forge distribution summary
jq '
  [.[].src.type]
  | group_by(.)
  | map({ (.[0]): length })
  | add
' "$OUTPUT_DIR/raw-sources.json" > "$OUTPUT_DIR/forge-breakdown.json"

# ------------------------------------------------------------------ #
# Stats
# ------------------------------------------------------------------ #
echo ""
echo "[+] Stats:"
jq 'length' "$OUTPUT_DIR/raw-sources.json"     | xargs printf "  Total packages  : %s\n"
jq 'length' "$OUTPUT_DIR/tag-refs.json"         | xargs printf "  Tag references  : %s\n"
jq 'length' "$OUTPUT_DIR/commit-refs.json"      | xargs printf "  Commit refs     : %s\n"
jq 'length' "$OUTPUT_DIR/svn-refs.json"         | xargs printf "  SVN revisions   : %s\n"
jq 'length' "$OUTPUT_DIR/no-rev-refs.json"      | xargs printf "  No VCS ref      : %s\n"
jq 'length' "$OUTPUT_DIR/github-refs.json"      | xargs printf "  GitHub packages : %s\n"
jq 'length' "$OUTPUT_DIR/gitlab-refs.json"      | xargs printf "  GitLab packages : %s\n"
jq 'length' "$OUTPUT_DIR/bitbucket-refs.json"   | xargs printf "  Bitbucket pkgs  : %s\n"
jq 'length' "$OUTPUT_DIR/sourcehut-refs.json"   | xargs printf "  Sourcehut pkgs  : %s\n"
jq 'length' "$OUTPUT_DIR/other-forge-refs.json" | xargs printf "  Other forge pkgs: %s\n"

echo ""
echo "[+] Tags by forge:"
jq -r '.[] | "  \(.forge): \(.count)"' "$OUTPUT_DIR/tags-by-forge.json"

echo ""
echo "[+] Done! Results in $OUTPUT_DIR/"
