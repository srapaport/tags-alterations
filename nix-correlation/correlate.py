# correlate_all_forges.py
import json, re, sqlite3
import pandas as pd
from pathlib import Path

# ------------------------------------------------------------------ #
# 1. Load Nix extracted data
# ------------------------------------------------------------------ #

with open("nix-sources/raw-sources.json") as f:
    raw = json.load(f)

records = []
for attr_path, pkg in raw.items():
    src = pkg.get("src", {})
    if not src:
        continue
    records.append({
        "attr_path":  attr_path,
        "pname":      pkg.get("pname"),
        "version":    pkg.get("version"),
        # source fields
        "src_type":   src.get("type"),
        "domain":     src.get("domain"),
        "owner":      src.get("owner"),
        "repo":       src.get("repo"),
        "rev":        src.get("rev"),
        "rev_type":   src.get("rev_type"),
        "tag_name":   src.get("tag_name"),   # already "refs/tags/…" or null
        "hash":       src.get("hash"),
        "origin_url": src.get("origin_url"), # matches your DB column directly
        "fetch_url":  src.get("fetch_url"),
    })

nix_df = pd.DataFrame(records)
print(f"Total Nix packages with sources : {len(nix_df):>8,}")
print(f"  tag-based refs                : {(nix_df.rev_type=='tag').sum():>8,}")
print(f"  commit-based refs             : {nix_df.rev_type.str.startswith('commit').sum():>8,}")
print(f"\nFetcher breakdown:\n{nix_df.src_type.value_counts().to_string()}")
print(f"\nDomain breakdown:\n{nix_df.domain.value_counts().head(20).to_string()}")

# ------------------------------------------------------------------ #
# 2. Load tag alteration DB
# ------------------------------------------------------------------ #

conn = sqlite3.connect("tags_alterations_full_2025-10_v2.db")
tags_df = pd.read_sql_query("""
    SELECT
        origin_url,
        tag_name,
        category,
        platform,
        old_snap_timestamp,
        new_snap_timestamp,
        old_revision,
        new_revision,
        rev_time_diff_days
    FROM tag_inconsistencies
""", conn)
conn.close()

print(f"\nTag inconsistency DB rows: {len(tags_df):,}")
print(f"  Alterations : {(tags_df.category=='Alteration').sum():,}")
print(f"  Deletions   : {(tags_df.category=='Deletion').sum():,}")

# ------------------------------------------------------------------ #
# 3. Normalise both sides for joining
# ------------------------------------------------------------------ #

# Strip refs/tags/ from the DB side so both sides use bare tag names
tags_df["tag_bare"] = (
    tags_df["tag_name"]
    .str.replace(r"^refs/tags/", "", regex=True)
    .str.strip()
)

# Nix side: strip refs/tags/ from tag_name too (already done in extractor,
# but rev is the bare name — use rev directly as join key)
nix_tags = nix_df[nix_df["rev_type"] == "tag"].copy()

# ------------------------------------------------------------------ #
# 4. Join on (origin_url, tag/rev)
# The Nix extractor builds origin_url to match your DB convention
# ------------------------------------------------------------------ #

merged = nix_tags.merge(
    tags_df,
    left_on  = ["origin_url", "rev"],
    right_on = ["origin_url", "tag_bare"],
    how      = "inner",
    suffixes = ("_nix", "_db"),
)

print(f"\n{'='*60}")
print(f"Nix packages whose pinned tag appears in alteration DB: {len(merged):,}")
print(f"Unique Nix attributes affected : {merged['attr_path'].nunique():,}")
print(f"Unique repos affected          : {merged['origin_url'].nunique():,}")

print(f"\nBreakdown by category:\n{merged['category'].value_counts().to_string()}")
print(f"\nBreakdown by platform:\n{merged['platform'].value_counts().to_string()}")
print(f"\nBreakdown by fetcher type:\n{merged['src_type'].value_counts().to_string()}")

# ------------------------------------------------------------------ #
# 5. Risk tiers
# ------------------------------------------------------------------ #

# Tier 1 — tag moved to a DIFFERENT commit (active content change)
tier1 = merged[merged["category"] == "Alteration"].copy()

# Tier 2 — tag deleted (reproducibility broken, but no content swap)
tier2 = merged[merged["category"] == "Deletion"].copy()

# Within Tier 1: was the hash update reflected in nixpkgs?
# (if sha256 stayed the same after tag move → nix build would FAIL,
#  meaning the alteration was caught; if different → silent swap possible)
# We can't know from static data alone, but we flag it for manual audit.

print(f"\n⚠️  TIER 1 — Tag content changed (highest risk): {len(tier1):,}")
print(tier1[[
    "attr_path", "version", "origin_url", "rev",
    "old_revision", "new_revision", "rev_time_diff_days"
]].head(20).to_string(index=False))

print(f"\n⚠️  TIER 2 — Tag deleted (reproducibility broken): {len(tier2):,}")
print(tier2[["attr_path", "version", "origin_url", "rev"]].head(20).to_string(index=False))

# ------------------------------------------------------------------ #
# 6. Export
# ------------------------------------------------------------------ #

merged.to_csv("nix-all-forge-affected-packages.csv", index=False)
tier1.to_json("tier1-tag-alterations.json",  orient="records", indent=2)
tier2.to_json("tier2-tag-deletions.json",    orient="records", indent=2)

# Per-platform summary table (useful for paper)
summary = (
    merged
    .groupby(["platform", "category", "src_type"])
    .agg(
        packages   = ("attr_path",  "nunique"),
        repos      = ("origin_url", "nunique"),
        mean_days  = ("rev_time_diff_days", "mean"),
    )
    .reset_index()
    .sort_values(["platform", "category"])
)
print(f"\nPer-platform summary:\n{summary.to_string(index=False)}")
summary.to_csv("per-platform-summary.csv", index=False)
