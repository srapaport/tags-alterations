#!/usr/bin/env bash
set -euo pipefail

# Build two archives for Zenodo publication:
# 1) code archive (without build artifacts)
# 2) data archive (precomputed artifacts, compressed)

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-${ROOT_DIR}/dist}"
CODE_ARCHIVE="${OUT_DIR}/tags-alterations-code.tar.gz"
DATA_ARCHIVE="${OUT_DIR}/tags-alterations-data.tar.gz"
VENV_PYTHON="${ROOT_DIR}/.venv/bin/python"
PY_ENV_DIR="${ROOT_DIR}/reproducibility/python-env"

mkdir -p "${OUT_DIR}"
mkdir -p "${PY_ENV_DIR}"

cd "${ROOT_DIR}"

if [[ ! -x "${VENV_PYTHON}" ]]; then
  echo "Error: expected Python environment at ${VENV_PYTHON}" >&2
  echo "Create .venv first or adapt scripts/create_zenodo_archives.sh." >&2
  exit 1
fi

echo "Exporting Python environment metadata from .venv"
"${VENV_PYTHON}" -VV > "${PY_ENV_DIR}/python-version.txt"
"${VENV_PYTHON}" -m pip freeze --all > "${PY_ENV_DIR}/requirements-lock.txt"
"${VENV_PYTHON}" -m pip list --format=json > "${PY_ENV_DIR}/pip-list.json"
"${VENV_PYTHON}" -m jupyter kernelspec list --json > "${PY_ENV_DIR}/kernelspecs.json"
uname -a > "${PY_ENV_DIR}/platform.txt"

echo "Creating code archive: ${CODE_ARCHIVE}"
tar -czf "${CODE_ARCHIVE}" \
  --exclude='target' \
  --exclude='.git' \
  --exclude='.github' \
  --exclude='.venv' \
  --exclude='.env' \
  --exclude='.gitignore' \
  --exclude='scripts/create_zenodo_archives.sh' \
  --exclude='dist' \
  Cargo.toml \
  Cargo.lock \
  src \
  notebooks \
  reproducibility \
  nix-correlation \
  scripts

echo "Creating data archive: ${DATA_ARCHIVE}"
tar -czf "${DATA_ARCHIVE}" data

echo "Done. Archives written to ${OUT_DIR}"
