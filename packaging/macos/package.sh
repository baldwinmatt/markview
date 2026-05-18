#!/usr/bin/env sh
set -eu

APP_NAME="Markview"
VERSION="${VERSION:-$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)}"
DIST_DIR="${DIST_DIR:-target/dist}"
BUNDLE_DIR="${BUNDLE_DIR:-target/macos/${APP_NAME}.app}"
ARCHIVE="${DIST_DIR}/markview-${VERSION}-macos.zip"

BUILD_MODE=release BUNDLE_DIR="${BUNDLE_DIR}" sh packaging/macos/bundle.sh

mkdir -p "${DIST_DIR}"
rm -f "${ARCHIVE}"
ditto -c -k --keepParent "${BUNDLE_DIR}" "${ARCHIVE}"

echo "Created ${ARCHIVE}"
