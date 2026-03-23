# correlate.py
import json
import pandas as pd
from pathlib import Path

# ------------------------------------------------------------------ #
# 1. Load Nix extracted sources
# ------------------------------------------------------------------ #
nix_path = sorted(Path(".").glob("nix-sources-*/tag-refs.json"))[-1]
print(f"[*] Loading Nix sources from: {nix_path}")

with open(nix_path) as f:
    nix_raw = json.load(f)

nix_df = pd.DataFrame([
    {
        "attr_path"  : pkg,
        "pname"      : data["pname"],
        "version"    : data["version"],
        "forge_type" : data["src"]["type"],
        "origin_url" : data["src"]["origin_url"],
        "owner"      : data["src"].get("owner"),
        "repo"       : data["src"].get("repo"),
        "rev"        : data["src"]["rev"],
        "rev_type"   : data["src"]["rev_type"],
        "hash"       : data["src"]["hash"],
        "fetch_url"  : data["src"].get("fetch_url"),
    }
    for pkg, data in nix_raw.items()
])

print(f"[*] Nix tag references loaded : {len(nix_df):,}")

# ------------------------------------------------------------------ #
# 2. Load tag alterations from pickle
# ------------------------------------------------------------------ #
PICKLE_PATH = Path("tags_df.pkl")

print(f"[*] Loading tag alterations from: {PICKLE_PATH}")
tags_df = pd.read_pickle(PICKLE_PATH)

print(f"[*] Tag alterations loaded    : {len(tags_df):,}")
print(f"[*] Unique repos in DB        : {tags_df['origin_url'].nunique():,}")

# ------------------------------------------------------------------ #
# 3. Step 1 — matching origins
# ------------------------------------------------------------------ #
shared_origins = set(nix_df["origin_url"]) & set(tags_df["origin_url"])

print(f"\n[Step 1] Repos in both datasets                : {len(shared_origins):,}")

step1_df = nix_df[nix_df["origin_url"].isin(shared_origins)].copy()

print(f"         Nix packages from those repos          : {len(step1_df):,}")
print(f"         Unique attr_paths                      : {step1_df['attr_path'].nunique():,}")

# ------------------------------------------------------------------ #
# 4. Step 2 — matching origins AND altered tag
# ------------------------------------------------------------------ #
step2_df = step1_df.merge(
    tags_df,
    left_on  = ["origin_url", "rev"],
    right_on = ["origin_url", "tag_bare"],
    how      = "inner",
).rename(columns={
    "tag_bare"    : "altered_tag",
    "old_snapshot"     : "sha_before_alteration",
    "new_snapshot"     : "sha_after_alteration",
    "new_snap_timestamp" : "alteration_detected_at",
})

print(f"\n[Step 2] Packages whose pinned tag was altered : {len(step2_df):,}")
print(f"         Unique attr_paths affected             : {step2_df['attr_path'].nunique():,}")
print(f"         Unique repos affected                  : {step2_df['origin_url'].nunique():,}")

# ------------------------------------------------------------------ #
# 5. Save results
# ------------------------------------------------------------------ #
OUTPUT_DIR = Path("correlation-results")
OUTPUT_DIR.mkdir(exist_ok=True)

step1_df.to_csv(OUTPUT_DIR  / "step1_shared_origins.csv",       index=False)
step1_df.to_json(OUTPUT_DIR / "step1_shared_origins.json",      orient="records", indent=2)

step2_df.to_csv(OUTPUT_DIR  / "step2_altered_tag_matches.csv",  index=False)
step2_df.to_json(OUTPUT_DIR / "step2_altered_tag_matches.json", orient="records", indent=2)

print(f"\n[+] Results written to {OUTPUT_DIR}/")

# ------------------------------------------------------------------ #
# 6. Preview
# ------------------------------------------------------------------ #
COLS = [
    "attr_path", "pname", "version",
    "origin_url", "rev",
    "sha_before_alteration", "sha_after_alteration",
    "alteration_detected_at", "fetch_url",
]

print("\n[Preview] First 10 affected packages:\n")
print(step2_df[COLS].head(10).to_string(index=False))
