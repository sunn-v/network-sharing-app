#!/usr/bin/env bash
# Register the app's bundle ID on Apple Developer portal + download
# Developer ID provisioning profiles: app/embedded.provisionprofile (app) and
# app/macos/LANDrive.provisionprofile (LAN Drive extension). Copied from GomokuCafe.
#
# Required env vars (set in .env.local at repo root):
#   APPLE_API_KEY_ID      — 10-char Key ID
#   APPLE_API_ISSUER_ID   — UUID issuer
#   APPLE_API_KEY_PATH    — absolute path to AuthKey_*.p8
#
# Idempotent: if App ID already exists, just re-downloads the profile.
# Re-run after changing bundle ID in tauri.conf.json — Fastlane will
# register the new ID and grab a new profile.

set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f .env.local ]; then
    set -a
    # shellcheck disable=SC1091
    source .env.local
    set +a
fi

: "${APPLE_API_KEY_ID:?APPLE_API_KEY_ID is not set (see scripts/provision-mac.sh header)}"
: "${APPLE_API_ISSUER_ID:?APPLE_API_ISSUER_ID is not set}"
: "${APPLE_API_KEY_PATH:?APPLE_API_KEY_PATH is not set}"

if [ ! -f "$APPLE_API_KEY_PATH" ]; then
    echo "✗ APPLE_API_KEY_PATH points to a missing file: $APPLE_API_KEY_PATH" >&2
    exit 1
fi

export APPLE_API_KEY_ID APPLE_API_ISSUER_ID APPLE_API_KEY_PATH
exec fastlane provision
