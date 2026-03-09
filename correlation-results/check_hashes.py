#!/usr/bin/env python3
"""
Try to re-fetch the source of each nixpkg in step2_altered_tag_matches.csv
and compare the resulting hash against what the nixpkg recorded.

A mismatch means the tag was silently altered after the package was pinned:
the build would now silently fetch different source code — a reproducibility
and supply-chain integrity issue.

Fetcher strategies
------------------
- fetchFromGitHub / fetchzip / fetchTarball / fetchurl
    → nix store prefetch-file --unpack --json <fetch_url>
      (nixpkgs hashes the *unpacked* tree, not the raw tarball)
- fetchgit
    → nix-prefetch-git --url <origin_url> --rev <rev> --fetch-submodules --json

Requirements
------------
- nix >= 2.4  (for `nix store prefetch-file`)
- nix-prefetch-git  (for fetchgit entries; part of nixpkgs)

Usage
-----
  python3 check_hashes.py [--input step2_altered_tag_matches.csv]
                          [--output hash_check_results.csv]
                          [--workers 4]
"""

import argparse
import json
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from typing import Optional

import pandas as pd
from tqdm import tqdm


# ---------------------------------------------------------------------------
# Fetching helpers
# ---------------------------------------------------------------------------

def _run(cmd: list[str], timeout: int) -> tuple[int, str, str]:
    """Run a subprocess and return (returncode, stdout, stderr)."""
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return proc.returncode, proc.stdout.strip(), proc.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1, "", f"Timed out after {timeout}s"
    except FileNotFoundError:
        return -1, "", f"Command not found: {cmd[0]}"


def _normalize_sri(h: str) -> str:
    """Normalize an SRI hash string for stable comparison.

    Strips whitespace and adds base-64 padding if missing so that
    'sha256-abc' and 'sha256-abc=' compare equal after normalisation.
    Both forms appear in nixpkgs.
    """
    h = h.strip()
    if not h:
        return h
    algo, _, b64 = h.partition("-")
    # Add padding to a multiple of 4 (base64 requires it)
    missing = (4 - len(b64) % 4) % 4
    return f"{algo}-{b64}{'=' * missing}"


def fetch_tarball_hash(fetch_url: str, timeout: int = 120) -> dict:
    """
    Fetch and unpack a tarball, returning the nix SRI hash of the unpacked
    directory.  This is the hash that fetchFromGitHub / fetchzip record in
    the nixpkg.

    Returns a dict with keys: hash (str|None), error (str|None)
    """
    rc, out, err = _run(
        ["nix", "store", "prefetch-file", "--unpack", "--json", fetch_url],
        timeout=timeout,
    )
    if rc != 0:
        return {"hash": None, "error": err or f"exit code {rc}"}
    try:
        data = json.loads(out)
        return {"hash": data.get("hash"), "error": None}
    except json.JSONDecodeError as e:
        return {"hash": None, "error": f"JSON parse error: {e} — stdout: {out}"}


def fetch_git_hash(origin_url: str, rev: str, timeout: int = 180) -> dict:
    """
    Clone a git repo at a specific rev and return the nix SRI hash of its
    tree.  This is the hash that fetchgit records in the nixpkg.

    Returns a dict with keys: hash (str|None), error (str|None)
    """
    rc, out, err = _run(
        [
            "nix-prefetch-git",
            "--url", origin_url,
            "--rev", rev,
            "--fetch-submodules",
            "--json",
        ],
        timeout=timeout,
    )
    if rc != 0:
        return {"hash": None, "error": err or f"exit code {rc}"}
    try:
        data = json.loads(out)
        # nix-prefetch-git outputs "hash" (SRI) in newer versions or "sha256" (base32) in older ones
        h = data.get("hash") or data.get("sha256")
        return {"hash": h, "error": None}
    except json.JSONDecodeError as e:
        return {"hash": None, "error": f"JSON parse error: {e} — stdout: {out}"}


# ---------------------------------------------------------------------------
# Per-row check
# ---------------------------------------------------------------------------

# Fetchers whose hash covers the *unpacked* tarball tree
TARBALL_FETCHERS = {
    "fetchFromGitHub",
    "fetchFromGitLab",
    "fetchFromGitea",
    "fetchFromForgejo",
    "fetchFromGogs",
    "fetchFromBitbucket",
    "fetchFromSourcehut",
    "fetchzip",
    "fetchTarball",
    "fetchurl",
}


def check_row(row: dict) -> dict:
    """
    Re-fetch the source for one nixpkg entry and compare hashes.

    Returns a result dict ready to be added to the output DataFrame.
    """
    def _str(v) -> str:
        return "" if v is None or (isinstance(v, float) and v != v) else str(v)

    attr_path    = _str(row.get("attr_path"))
    forge_type   = _str(row.get("forge_type"))
    stored_hash  = _str(row.get("hash"))
    fetch_url    = _str(row.get("fetch_url"))
    origin_url   = _str(row.get("origin_url"))
    rev          = _str(row.get("rev"))

    base = {
        "attr_path":         attr_path,
        "forge_type":        forge_type,
        "stored_hash":       stored_hash,
        "fetched_hash":      None,
        "hash_match":        None,   # True / False / None (error)
        "fetch_error":       None,
        "checked_at":        datetime.now(timezone.utc).isoformat(),
    }

    if forge_type in TARBALL_FETCHERS:
        if not fetch_url:
            base["fetch_error"] = "No fetch_url available"
            return base
        result = fetch_tarball_hash(fetch_url)

    elif forge_type == "fetchgit":
        if not origin_url or not rev:
            base["fetch_error"] = "Missing origin_url or rev for fetchgit"
            return base
        result = fetch_git_hash(origin_url, rev)

    else:
        base["fetch_error"] = f"Unsupported forge_type: {forge_type}"
        return base

    fetched = result["hash"]
    error   = result["error"]

    base["fetched_hash"] = fetched
    base["fetch_error"]  = error

    if fetched and stored_hash:
        base["hash_match"] = _normalize_sri(fetched) == _normalize_sri(stored_hash)

    return base


# ---------------------------------------------------------------------------
# Main processing
# ---------------------------------------------------------------------------

def process(input_csv: str, output_csv: str, workers: int):
    df = pd.read_csv(input_csv)
    print(f"Loaded {len(df)} entries from {input_csv}")

    rows = df.to_dict(orient="records")

    results: list[Optional[dict]] = [None] * len(rows)
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(check_row, row): i for i, row in enumerate(rows)}
        with tqdm(total=len(rows), desc="Checking hashes") as pbar:
            for future in as_completed(futures):
                results[futures[future]] = future.result()
                pbar.update(1)

    out_df = pd.DataFrame(results)
    out_df.to_csv(output_csv, index=False)
    print(f"\nResults saved to {output_csv}")

    # Summary
    total   = len(out_df)
    matched = out_df["hash_match"].sum()
    mismatched = (out_df["hash_match"] == False).sum()  # noqa: E712
    errors  = out_df["fetch_error"].notna().sum()
    unknown = out_df["hash_match"].isna().sum()

    print("\n" + "=" * 60)
    print("SUMMARY")
    print("=" * 60)
    print(f"Total packages checked : {total}")
    print(f"  Hash MATCH           : {int(matched)}  — tag unchanged (or alteration had no effect on tarball)")
    print(f"  Hash MISMATCH        : {int(mismatched)}  — reproducibility issue confirmed ✗")
    print(f"  Fetch error / unknown: {errors} / {unknown}")

    if mismatched > 0:
        print("\nMismatched packages:")
        bad = out_df[out_df["hash_match"] == False][["attr_path", "stored_hash", "fetched_hash"]]  # noqa: E712
        print(bad.to_string(index=False))


def main():
    parser = argparse.ArgumentParser(
        description="Re-fetch nix sources and check for hash mismatches caused by tag alterations"
    )
    parser.add_argument(
        "--input",
        default="step2_altered_tag_matches.csv",
        help="Input CSV (default: step2_altered_tag_matches.csv)",
    )
    parser.add_argument(
        "--output",
        default="step3_hash_check_results.csv",
        help="Output CSV (default: step3_hash_check_results.csv)",
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=4,
        help="Parallel workers (default: 4; keep low to avoid hammering forges)",
    )
    args = parser.parse_args()
    process(args.input, args.output, args.workers)


if __name__ == "__main__":
    main()
