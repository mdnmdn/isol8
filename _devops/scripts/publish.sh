#!/usr/bin/env bash
# Publish the isol8 workspace to crates.io in dependency order.
#
#   publish.sh            — publish every crate that is not already on crates.io
#   publish.sh --dry-run  — package + verify each crate, upload nothing
#
# Why this exists: the root `isol8` package depends on `isol8-core`,
# `isol8-registry` and `isol8-cli` by path *and* version. crates.io resolves the
# version requirement against the registry, not the path, so a bare
# `cargo publish` at the root fails with
#
#     error: no matching package named `isol8-cli` found
#
# Members must therefore be published first, leaves before roots.
#
# Idempotent: a crate whose exact version is already on crates.io is skipped, so
# a re-run after a partial failure resumes instead of erroring out.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

# Dependency order: isol8-core → isol8-registry → isol8-cli → isol8 (facade).
# isol8-registry depends on core; isol8-cli on both; the facade on all three.
CRATES=(isol8-core isol8-registry isol8-cli isol8)

DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN=1
elif [[ $# -gt 0 ]]; then
  echo "usage: publish.sh [--dry-run]" >&2
  exit 1
fi

version_of() {
  cargo metadata --no-deps --format-version 1 \
    | python3 -c "
import json,sys
name = sys.argv[1]
for p in json.load(sys.stdin)['packages']:
    if p['name'] == name:
        print(p['version'])
        break
" "$1"
}

already_published() {
  local name="$1" version="$2"
  curl -sSf "https://crates.io/api/v1/crates/${name}/${version}" >/dev/null 2>&1
}

# crates.io indexes asynchronously; a dependent crate can fail to resolve the one
# just published. Modern cargo waits, but not forever — poll before moving on.
wait_for_index() {
  local name="$1" version="$2"
  local tries=0
  until already_published "${name}" "${version}"; do
    tries=$((tries + 1))
    if [[ ${tries} -ge 30 ]]; then
      echo "error: ${name}@${version} did not appear on crates.io after 5min" >&2
      exit 1
    fi
    echo "  waiting for ${name}@${version} to index (${tries}/30)…"
    sleep 10
  done
  echo "  ${name}@${version} is live"
}

echo "isol8 workspace publish (order: ${CRATES[*]})"
[[ ${DRY_RUN} -eq 1 ]] && echo "DRY RUN — nothing will be uploaded"
echo

UNVERIFIED=()
FAILED=()

for crate in "${CRATES[@]}"; do
  version="$(version_of "${crate}")"
  if [[ -z "${version}" ]]; then
    echo "error: could not determine version for ${crate}" >&2
    exit 1
  fi

  if [[ ${DRY_RUN} -eq 1 ]]; then
    echo "==> ${crate}@${version} (dry run)"
    if already_published "${crate}" "${version}"; then
      echo "  already on crates.io — publish would skip"
      continue
    fi
    # A crate cannot be packaged until its siblings are on crates.io, because
    # cargo resolves the version requirement against the registry. Before the
    # first release that is expected, not a failure — but anything else is.
    if err="$(cargo package -p "${crate}" --allow-dirty --quiet 2>&1)"; then
      echo "  packages cleanly"
    elif grep -q 'no matching package named `isol8-' <<<"${err}"; then
      missing="$(sed -n 's/.*no matching package named `\([^`]*\)`.*/\1/p' <<<"${err}" | head -1)"
      echo "  cannot verify yet — ${missing} is not on crates.io"
      UNVERIFIED+=("${crate}")
    else
      echo "  FAILED:" >&2
      sed 's/^/    /' <<<"${err}" >&2
      FAILED+=("${crate}")
    fi
    continue
  fi

  if already_published "${crate}" "${version}"; then
    echo "==> ${crate}@${version} already published — skipping"
    continue
  fi

  echo "==> publishing ${crate}@${version}"
  cargo publish -p "${crate}"
  wait_for_index "${crate}" "${version}"
done

echo
if [[ ${DRY_RUN} -eq 1 ]]; then
  if [[ ${#FAILED[@]} -gt 0 ]]; then
    echo "FAILED to package: ${FAILED[*]}" >&2
    exit 1
  fi
  if [[ ${#UNVERIFIED[@]} -gt 0 ]]; then
    echo "dry run incomplete — could not verify: ${UNVERIFIED[*]}"
    echo "  (expected before the first release: each crate becomes verifiable"
    echo "   once the one below it is on crates.io. Run the real publish to"
    echo "   bootstrap, or re-run this after isol8-core lands.)"
    exit 0
  fi
  echo "ok: every crate packages cleanly"
  exit 0
fi

echo "ok: workspace published"
