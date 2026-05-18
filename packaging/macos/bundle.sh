#!/usr/bin/env sh
set -eu

APP_NAME="Markview"
BINARY_NAME="markview-gui"
BUNDLE_DIR="${BUNDLE_DIR:-target/macos/${APP_NAME}.app}"
BUILD_MODE="${BUILD_MODE:-debug}"
FEATURES="${FEATURES:-gui}"

case "${BUILD_MODE}" in
  debug)
    CARGO_ARGS=""
    BINARY_DIR="target/debug"
    ;;
  release)
    CARGO_ARGS="--release"
    BINARY_DIR="target/release"
    ;;
  *)
    echo "BUILD_MODE must be debug or release" >&2
    exit 2
    ;;
esac

cargo build ${CARGO_ARGS} --features "${FEATURES}" --bin "${BINARY_NAME}"

rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}/Contents/MacOS" "${BUNDLE_DIR}/Contents/Resources"
cp "packaging/macos/Info.plist" "${BUNDLE_DIR}/Contents/Info.plist"
cp "${BINARY_DIR}/${BINARY_NAME}" "${BUNDLE_DIR}/Contents/MacOS/${APP_NAME}"
base64 -D -i "packaging/macos/Markview.icns.base64" -o "${BUNDLE_DIR}/Contents/Resources/Markview.icns"

echo "Created ${BUNDLE_DIR}"
