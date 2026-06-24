#!/usr/bin/env bash
# Package isol8 release binaries for GitHub Releases.
#
# Usage: package-release.sh <rust-target> <os-label> <arch-label>
# Example: package-release.sh x86_64-pc-windows-gnu windows x64
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: package-release.sh <target> <os> <arch>" >&2
  exit 1
fi

TARGET="$1"
OS="$2"
ARCH="$3"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

ARTIFACT="target/${TARGET}/release"
OUT_ZIP="${OS}-${ARCH}.zip"

rm -rf dist
mkdir -p dist

case "${TARGET}" in
  *-windows-*)
    BIN="${ARTIFACT}/isol8.exe"
    HOOK_SRC="${ARTIFACT}/isol8_winhook.dll"
    HOOK_DST="dist/isol8-winhook.dll"

    [[ -f "${BIN}" ]] || {
      echo "error: missing ${BIN}" >&2
      exit 1
    }
    [[ -f "${HOOK_SRC}" ]] || {
      echo "error: missing ${HOOK_SRC} (build isol8-winhook for Windows releases)" >&2
      exit 1
    }

    cp "${BIN}" dist/
    cp "${HOOK_SRC}" "${HOOK_DST}"
    ;;
  *)
    BIN="${ARTIFACT}/isol8"
    [[ -f "${BIN}" ]] || {
      echo "error: missing ${BIN}" >&2
      exit 1
    }
    cp "${BIN}" dist/
    ;;
esac

rm -f "${OUT_ZIP}"
(
  cd dist
  if command -v zip >/dev/null 2>&1; then
    if [[ "${OS}" == "windows" ]]; then
      zip "../${OUT_ZIP}" isol8.exe isol8-winhook.dll
    else
      zip "../${OUT_ZIP}" isol8
    fi
  else
    # GitHub windows-latest: Compress-Archive when zip(1) is absent.
    if [[ "${OS}" == "windows" ]]; then
      powershell.exe -NoProfile -Command \
        "Compress-Archive -Path 'isol8.exe','isol8-winhook.dll' -DestinationPath '../${OUT_ZIP}' -Force"
    else
      echo "error: zip(1) required to package ${OUT_ZIP}" >&2
      exit 1
    fi
  fi
)

echo "packaged ${OUT_ZIP}"
ls -la "${OUT_ZIP}" dist/