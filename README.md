# Replication Package

This package reproduces the article workflow and supports a precomputed-data path for fast evaluation.

Use this README for end-to-end Rust pipeline reproduction and optional Nix correlation checks.

## Package Layout

- src/: Rust pipeline and binaries.
- data/: precomputed artifacts kept for notebook execution.
- data/counts/: precomputed count and frequency logs.
- notebooks/: retained notebooks only.
- nix-correlation/: optional independent workflow and precomputed correlation outputs.

## Notebook Roles

- build_datasets.ipynb: builds/refreshes intermediate datasets used downstream.
- quantitative.ipynb: computes quantitative analysis outputs.
- analyze_tags_figures.ipynb: generates figure-oriented outputs.

## Prerequisites

- Rust toolchain (cargo, rustc)
- Python 3.10+
- Optional for Nix workflow: nix and nix-prefetch-git
- Access to Software Heritage graph files and ORC origin visit status directory

## Skip Option: Precomputed Notebook Path

If you want to skip Rust recomputation and directly evaluate the precomputed results:

1) Keep `data/` and `data/counts/` as provided.
2) Activate the Python environment from the lock file:

```bash
python -m venv .venv
. .venv/bin/activate
pip install -r reproducibility/python-env/requirements-lock.txt
```

3) Run notebooks in this order:

- `notebooks/quantitative.ipynb`
- `notebooks/analyze_tags_figures.ipynb`

This skip path is intended for fast reproducibility checks at evaluation time.

## Required Inputs

Provide these paths explicitly (CLI flags or environment variables):

- Graph basename path
- ORC origin visit status directory
- SQLite database path (output)
- Dataset suffix string

Optional input:

- stars table name for snapshots_frequency_stats

## Build

```bash
cargo build --release
```

## Required Reproduction Workflow (ordered)

Run from repository root.

1) Main extraction and inconsistency detection

```bash
cargo run --release -- \
  --graph-basename /path/to/graph \
  --orc-dir /path/to/orc/origin_visit_status \
  --suffix full_2025-10_v2 \
  --db-path data/tags_alterations_full_2025-10_v2.db
```

2) Extended deletion and recreation detection

```bash
cargo run --release --bin extended_deletion_move -- \
  --graph-basename /path/to/graph \
  --orc-dir /path/to/orc/origin_visit_status \
  --suffix full_2025-10_v2 \
  --db-path data/tags_alterations_full_2025-10_v2.db
```

3) History check

```bash
cargo run --release --bin history_check -- \
  --graph-basename /path/to/graph \
  --suffix full_2025-10_v2 \
  --db-path data/tags_alterations_full_2025-10_v2.db
```

4) Extended release timestamp checks

```bash
cargo run --release --bin extended_release_ts -- \
  --graph-basename /path/to/graph \
  --suffix full_2025-10_v2 \
  --db-path data/tags_alterations_full_2025-10_v2.db
```

5) Notebook stage: run `notebooks/build_datasets.ipynb`

This notebook must run before `classification` and `snapshots_frequency_stats` in your selected workflow.

6) Classification (after build_datasets notebook)

```bash
cargo run --release --bin classification -- \
  --graph-basename /path/to/graph \
  --suffix full_2025-10_v2 \
  --db-path data/tags_alterations_full_2025-10_v2.db
```

7) Snapshot frequency statistics (after build_datasets notebook)

```bash
cargo run --release --bin snapshots_frequency_stats -- \
  --orc-dir /path/to/orc/origin_visit_status \
  --suffix full_2025-10_v2 \
  --db-path data/tags_alterations_full_2025-10_v2.db \
  --stars-table tags_with_stars
```

## Optional Binaries

These are not required for the core ordered replication chain.

- count_tags
- count_repos
- count_commits_per_month
- first_release

Commands:

```bash
cargo run --release --bin count_tags
cargo run --release --bin count_repos
cargo run --release --bin count_commits_per_month
cargo run --release --bin first_release
```

## Optional Nix Correlation Workflow

This workflow is independent and can be run separately.

1) Correlate tags with Nix sources

```bash
cd nix-correlation
./run-extact-iterative.sh 08ffd21f7c8f90e15216bc329855b66ffea8526d
python correlate.py
```

2) Optional validation checks

```bash
python check_hashes.py \
    --input correlation-results/step2_altered_tag_matches.csv \
    --output correlation-results/step3_hash_check_results.csv
python check_builds.py \
    --input correlation-results/step2_altered_tag_matches.csv \
    --output correlation-results/step3_build_check_results.csv
```

Expected outputs are written in `nix-correlation/correlation-results/`.

## Reproducibility Notes

- Precomputed artifacts in data/ are kept for rapid evaluation.
- Full recomputation requires graph and ORC input paths.
    - 2025-10-08 graph weighs 492G
    - ORC files origin_visit_status weigh 89G
- Python notebook reproducibility files are in `reproducibility/python-env/`.

To enable the same Python environment for notebook reproduction:

```bash
python -m venv .venv
. .venv/bin/activate
pip install -r reproducibility/python-env/requirements-lock.txt
```

## Zenodo Publication Layout

- Code archive: source files and notebooks (do not include `target/`)
- Data archive: `data/` and `data/counts/` as tar.gz

```bash
tar -czf tags-alterations-code.tar.gz \
  Cargo.toml Cargo.lock src notebooks reproducibility nix-correlation README.md

tar -czf tags-alterations-data.tar.gz data
```
