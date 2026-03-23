#!/usr/bin/env python3
"""
Attempt to build each nixpkg in step2_altered_tag_matches.csv from source
(bypassing the binary cache) and inspect the output for hash mismatch errors.

This is stronger than check_hashes.py: instead of just re-downloading the
tarball and comparing hashes, it invokes nix itself to evaluate and fetch the
derivation.  If the tag was silently altered after the package was pinned, nix
will abort with a "hash mismatch in fixed-output derivation" error, which this
script detects and records.

Possible outcomes per package
------------------------------
  hash_mismatch  — nix detected a mismatch → reproducibility issue confirmed ✗
  build_error    — fetch succeeded but compilation failed (unrelated to alteration)
  success        — build completed (alteration had no effect, or tag is unchanged)
  eval_error     — nix could not evaluate the attribute (attr_path not found, etc.)
  timeout        — build exceeded the time limit
  unknown        — something else went wrong

Requirements
------------
- nix >= 2.4  (Determinate Nix 2.33+ works)
- A nixpkgs checkout OR internet access to pull <nixpkgs> from the registry

Usage
-----
  # Use the system nixpkgs channel:
  python3 check_builds.py

  # Point at a specific nixpkgs clone (recommended for reproducibility):
  python3 check_builds.py --nixpkgs /path/to/nixpkgs

  # Limit build time per package and increase parallelism:
  python3 check_builds.py --nixpkgs /path/to/nixpkgs --timeout 300 --workers 2
"""

import argparse
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from typing import Optional

import pandas as pd
from tqdm import tqdm


# ---------------------------------------------------------------------------
# Outcome classification
# ---------------------------------------------------------------------------

# nix error message emitted when the fetched content doesn't match the stored hash
_HASH_MISMATCH_RE = re.compile(
    r"hash mismatch in fixed.output derivation", re.IGNORECASE
)
# nix error when the attribute path doesn't exist in nixpkgs
_EVAL_ERROR_RE = re.compile(
    r"error: attribute '.*?' missing"
    r"|does not provide attribute"
    r"|evaluation aborted"
    r"|infinite recursion",
    re.IGNORECASE,
)
# nix error when the package is not supported on the current platform
_UNSUPPORTED_PLATFORM_RE = re.compile(
    r"not available on the requested hostPlatform"
    r"|package\.meta\.platforms",
    re.IGNORECASE,
)


def classify(returncode: int, stderr: str) -> str:
    if returncode == 0:
        return "success"
    if _HASH_MISMATCH_RE.search(stderr):
        return "hash_mismatch"
    if _UNSUPPORTED_PLATFORM_RE.search(stderr):
        return "unsupported_platform"
    if _EVAL_ERROR_RE.search(stderr):
        return "eval_error"
    if returncode == -1:
        return "timeout"
    return "build_error"


# ---------------------------------------------------------------------------
# Per-package build attempt
# ---------------------------------------------------------------------------

def build_package(attr_path: str, nixpkgs_ref: str, timeout: int) -> dict:
    """
    Run `nix build <nixpkgs_ref>#<attr_path>.src --rebuild --no-link`
    and return a result dict.

    Building only the `.src` attribute fetches and hash-checks the upstream
    source without compiling anything — orders of magnitude faster than a
    full build, and sufficient to detect hash mismatches caused by tag
    alterations.

    --rebuild        forces nix to re-execute the fetcher even when the output
                     is already in the local store or a substituter cache.
                     For fixed-output derivations, nix compares the hash of the
                     freshly-fetched content against the hash baked into the
                     nixpkgs expression; if they differ it aborts with:
                       "hash mismatch in fixed-output derivation"
                     Build tool dependencies (curl, etc.) are still allowed to
                     come from the cache, so there is no perl-from-scratch problem.
    --no-link        skips creating a ./result symlink in the cwd.
    """
    cmd = [
        "nix", "build",
        f"{nixpkgs_ref}#{attr_path}.src",
        "--rebuild",
        "--no-link",
        "--impure",
        # Print verbose fetch output so we can capture hash mismatch details
        "-L",
    ]

    env = dict(os.environ)
    env["NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM"] = "1"

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
        rc      = proc.returncode
        stderr  = proc.stderr.strip()
        stdout  = proc.stdout.strip()
    except subprocess.TimeoutExpired:
        rc     = -1
        stderr = f"Timed out after {timeout}s"
        stdout = ""

    outcome = classify(rc, stderr)

    # Extract the "got: sha256-..." line from the mismatch error if present
    got_hash = None
    m = re.search(r"got:\s+(sha256-\S+)", stderr)
    if m:
        got_hash = m.group(1)

    return {
        "attr_path":    attr_path,
        "outcome":      outcome,
        "returncode":   rc,
        "got_hash":     got_hash,      # hash nix actually fetched (on mismatch)
        "stderr_tail":  stderr[-2000:] if stderr else "",  # last 2 KB for diagnosis
        "checked_at":   datetime.now(timezone.utc).isoformat(),
    }


# ---------------------------------------------------------------------------
# Main processing
# ---------------------------------------------------------------------------

def process(input_csv: str, output_csv: str, nixpkgs_ref: str,
            timeout: int, workers: int):
    df = pd.read_csv(input_csv)
    print(f"Loaded {len(df)} entries from {input_csv}")
    print(f"nixpkgs reference : {nixpkgs_ref}")
    print(f"Fetch timeout     : {timeout}s per package")
    print(f"Workers           : {workers}")
    print()

    # Deduplicate by attr_path — one build per package is enough
    unique_attrs = df["attr_path"].unique().tolist()
    print(f"Unique attr_paths to build: {len(unique_attrs)}")

    results: list[Optional[dict]] = [None] * len(unique_attrs)
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {
            executor.submit(build_package, attr, nixpkgs_ref, timeout): i
            for i, attr in enumerate(unique_attrs)
        }
        with tqdm(total=len(unique_attrs), desc="Fetching sources") as pbar:
            for future in as_completed(futures):
                results[futures[future]] = future.result()
                pbar.update(1)

    out_df = pd.DataFrame(results)

    # Merge back with original data to keep all metadata
    out_df = df.merge(out_df, on="attr_path", how="left")
    out_df.to_csv(output_csv, index=False)
    print(f"\nResults saved to {output_csv}")

    # Summary
    counts = out_df["outcome"].value_counts()
    print("\n" + "=" * 60)
    print("SUMMARY")
    print("=" * 60)
    for outcome, n in counts.items():
        marker = " ✗  ← reproducibility issue confirmed" if outcome == "hash_mismatch" else ""
        print(f"  {outcome:<15}: {n}{marker}")

    mismatched = out_df[out_df["outcome"] == "hash_mismatch"]
    if not mismatched.empty:
        print("\nPackages with hash mismatch:")
        print(mismatched[["attr_path", "stored_hash" if "stored_hash" in out_df.columns else "hash", "got_hash"]].to_string(index=False))


def main():
    parser = argparse.ArgumentParser(
        description="Fetch nixpkgs package sources and detect hash mismatches caused by tag alterations"
    )
    parser.add_argument(
        "--input",
        default="step2_altered_tag_matches.csv",
        help="Input CSV (default: step2_altered_tag_matches.csv)",
    )
    parser.add_argument(
        "--output",
        default="step3_build_check_results.csv",
        help="Output CSV (default: step3_build_check_results.csv)",
    )
    parser.add_argument(
        "--nixpkgs",
        default="nixpkgs",
        help=(
            "nixpkgs reference for `nix build <ref>#attr`. "
            "Can be a local path (/path/to/nixpkgs), a flake ref "
            "(github:NixOS/nixpkgs/master), or 'nixpkgs' to use the "
            "registry default (default: nixpkgs)"
        ),
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=120,
        help="Max seconds per fetch (default: 120)",
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=8,
        help=(
            "Parallel fetches (default: 8). Fetch-only is mostly network I/O "
            "so higher concurrency is safe; raise further if your connection "
            "allows it, but keep in mind forge rate limits."
        ),
    )
    args = parser.parse_args()

    # If the user gave a bare filesystem path, convert to an absolute path so
    # nix treats it as a flake path rather than a registry name.
    nixpkgs_ref = args.nixpkgs
    if os.path.isdir(nixpkgs_ref):
        nixpkgs_ref = os.path.abspath(nixpkgs_ref)

    process(args.input, args.output, nixpkgs_ref, args.timeout, args.workers)


if __name__ == "__main__":
    main()
