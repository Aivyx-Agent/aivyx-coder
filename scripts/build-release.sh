#!/bin/bash
set -euo pipefail

TARGET="x86_64-unknown-linux-musl"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Building aivyx-coder v${VERSION} for ${TARGET}..."

BUILD_LOG="$(mktemp)"
trap 'rm -f "${BUILD_LOG}"' EXIT

if ! cargo build --release --target "${TARGET}" -p aivyx 2>&1 | tee "${BUILD_LOG}"; then
  if grep -q "may not be installed" "${BUILD_LOG}"; then
    echo ""
    echo "ERROR: the ${TARGET} Rust target is not installed."
    echo "On Arch/CachyOS: pacman -S rust-musl"
    echo "On rustup-managed toolchains: rustup target add ${TARGET}"
    exit 1
  fi
  echo ""
  echo "ERROR: build failed. See output above."
  exit 1
fi

BINARY="target/${TARGET}/release/aivyx"
if [ ! -f "${BINARY}" ]; then
  echo "ERROR: expected binary not found at ${BINARY}"
  exit 1
fi

DIST_DIR="dist"
STAGE_NAME="aivyx-coder-v${VERSION}-x86_64-linux-musl"
STAGE_DIR="${DIST_DIR}/${STAGE_NAME}"
TARBALL_NAME="${STAGE_NAME}.tar.gz"

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}"
cp "${BINARY}" "${STAGE_DIR}/aivyx"
cp README.md LICENSE-MIT LICENSE-APACHE "${STAGE_DIR}/"

tar -czf "${DIST_DIR}/${TARBALL_NAME}" -C "${DIST_DIR}" "${STAGE_NAME}"
rm -rf "${STAGE_DIR}"

(cd "${DIST_DIR}" && sha256sum "${TARBALL_NAME}" > "${TARBALL_NAME}.sha256")

echo ""
echo "Built: ${DIST_DIR}/${TARBALL_NAME}"
echo "Checksum: ${DIST_DIR}/${TARBALL_NAME}.sha256"
