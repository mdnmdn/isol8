#!/usr/bin/env bash
# Version helpers for isol8 releases.
#
#   bump <version>   — validate, lint+test, update Cargo.toml + Cargo.lock, commit and tag
#   verify [tag]     — ensure tag (e.g. v0.3.0) matches Cargo.toml (for CI)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
CARGO_TOML="${REPO_ROOT}/Cargo.toml"
CARGO_LOCK="${REPO_ROOT}/Cargo.lock"

usage() {
  cat <<'EOF' >&2
Usage:
  version.sh bump <version>   # e.g. 0.3.0
  version.sh verify [tag]     # e.g. v0.3.0 (defaults to GITHUB_REF_NAME)
EOF
  exit 1
}

cargo_version() {
  sed -n 's/^version = "\(.*\)"/\1/p' "${CARGO_TOML}" | head -1
}

validate_version_format() {
  local version="$1"
  if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must be n.n.n (e.g. 0.3.0), got: ${version}" >&2
    exit 1
  fi
}

update_cargo_version() {
  local version="$1"
  local tmp
  tmp="$(mktemp)"
  sed "s/^version = .*/version = \"${version}\"/" "${CARGO_TOML}" >"${tmp}"
  mv "${tmp}" "${CARGO_TOML}"
}

cmd_bump() {
  local version="$1"
  validate_version_format "${version}"

  cd "${REPO_ROOT}"

  if git rev-parse "v${version}" >/dev/null 2>&1; then
    echo "error: tag v${version} already exists" >&2
    exit 1
  fi

  if ! git diff --cached --quiet; then
    echo "error: staged changes present; commit or unstage before bumping" >&2
    exit 1
  fi

  echo "running lint..."
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features -- -D warnings

  echo "running tests..."
  cargo test

  local current
  current="$(cargo_version)"
  if [[ "${current}" == "${version}" ]]; then
    echo "error: Cargo.toml already at version ${version}" >&2
    exit 1
  fi

  echo "updating Cargo.toml: ${current} -> ${version}"
  update_cargo_version "${version}"

  echo "syncing Cargo.lock..."
  cargo generate-lockfile --quiet

  git add "${CARGO_TOML}" "${CARGO_LOCK}"
  git commit -m "chore: release v${version}"
  git tag -a "v${version}" -m "v${version}"

  echo "ok: committed and tagged v${version}"
  echo "push with: git push && git push origin v${version}"
}

cmd_verify() {
  local tag="${1:-${GITHUB_REF_NAME:-}}"
  if [[ -z "${tag}" ]]; then
    echo "error: tag required (argument or GITHUB_REF_NAME)" >&2
    exit 1
  fi

  if [[ ! "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: tag must be vn.n.n (e.g. v0.3.0), got: ${tag}" >&2
    exit 1
  fi

  local expected_version="${tag#v}"
  local cargo_ver
  cargo_ver="$(cargo_version)"

  if [[ "${cargo_ver}" != "${expected_version}" ]]; then
    echo "error: tag ${tag} expects Cargo.toml version ${expected_version}, found ${cargo_ver}" >&2
    exit 1
  fi

  echo "ok: tag ${tag} matches Cargo.toml version ${cargo_ver}"
}

main() {
  [[ $# -ge 1 ]] || usage

  case "$1" in
    bump)
      [[ $# -eq 2 ]] || usage
      cmd_bump "$2"
      ;;
    verify)
      cmd_verify "${2:-}"
      ;;
    -h | --help | help)
      usage
      ;;
    *)
      echo "error: unknown command: $1" >&2
      usage
      ;;
  esac
}

main "$@"