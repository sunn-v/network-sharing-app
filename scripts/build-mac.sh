#!/usr/bin/env bash
# Build a signed + notarized .dmg for macOS distribution.
# Copied from getappscafe/GomokuCafe; adapted to this repo (app/ instead of src-tauri/,
# workspace target/ at the root) and to embed the LAN Drive File Provider extension.
#
# Prerequisites — see README "Build macOS":
#   1. Apple Developer Program membership ($99/yr)
#   2. "Developer ID Application" certificate installed in Keychain
#   3. Notary credentials stored in Keychain via:
#        xcrun notarytool store-credentials "<productName>Notary" ...
#   4. .env.local at repo root containing:
#        NOTARY_KEYCHAIN_PROFILE="<productName>Notary"
#   5. Xcode, xcodegen (brew install xcodegen), tauri-cli (cargo install tauri-cli)

set -euo pipefail

cd "$(dirname "$0")/.."

# Auto-load .env.local (gitignored)
if [ -f .env.local ]; then
    set -a
    # shellcheck disable=SC1091
    source .env.local
    set +a
fi

# Derive expected notary-profile name from productName so error
# messages show a command the user can copy-paste.
PRODUCT=$(grep '"productName"' app/tauri.conf.json 2>/dev/null \
    | head -1 \
    | sed -E 's/.*"productName"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
PROFILE_NAME="${PRODUCT:-Tauri}Notary"

# Optional --target=<triple> or --target <triple>
TARGET="universal-apple-darwin"
while [ $# -gt 0 ]; do
    case "$1" in
        --target=*) TARGET="${1#--target=}"; shift ;;
        --target)   shift; TARGET="${1:-}"; shift ;;
        *)          shift ;;
    esac
done

CARGO_ARGS=()
BUNDLE_BASE="target/release/bundle"
if [ -n "$TARGET" ]; then
    CARGO_ARGS+=(--target "$TARGET")
    BUNDLE_BASE="target/$TARGET/release/bundle"
    echo "→ Target: $TARGET"
fi

# ─── Resolve signing identity ────────────────────────────────────

if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
    APPLE_SIGNING_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
        | grep "Developer ID Application:" \
        | head -n 1 \
        | sed -E 's/.*"([^"]+)".*/\1/' || true)
fi

if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
    cat >&2 <<'EOF'
✗ No "Developer ID Application" certificate found in Keychain.

  See README → "Build macOS" for setup steps.
EOF
    exit 1
fi
export APPLE_SIGNING_IDENTITY
echo "→ Signing as: $APPLE_SIGNING_IDENTITY"

# The app reads APPLE_TEAM_ID at compile time to find the App Group it shares with the
# LAN Drive extension. Default: the "(XXXXXXXXXX)" at the end of the identity.
APPLE_TEAM_ID="${APPLE_TEAM_ID:-$(sed -nE 's/.*\(([A-Z0-9]{10})\)$/\1/p' <<<"$APPLE_SIGNING_IDENTITY")}"
if [ -z "$APPLE_TEAM_ID" ]; then
    echo "✗ Couldn't read the Team ID from '$APPLE_SIGNING_IDENTITY'. Set APPLE_TEAM_ID." >&2
    exit 1
fi
export APPLE_TEAM_ID

# ─── Verify notary credentials present ───────────────────────────

HAS_NOTARY=false
if [ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]; then
    HAS_NOTARY=true
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    HAS_NOTARY=true
fi

if ! $HAS_NOTARY; then
    if [ ! -t 0 ] || [ ! -t 1 ]; then
        cat >&2 <<EOF
✗ No notary credentials found (non-interactive shell).

  One-time setup:
    xcrun notarytool store-credentials "$PROFILE_NAME" \\
      --apple-id you@example.com \\
      --team-id XXXXXXXXXX \\
      --password xxxx-xxxx-xxxx-xxxx

  Then add to .env.local:
    NOTARY_KEYCHAIN_PROFILE="$PROFILE_NAME"

  See README → "Build macOS" for details.
EOF
        exit 1
    fi

    cat <<EOF

→ No notary credentials configured. Let's set them up (one-time).

  You'll need:
    1. Apple ID email
    2. Team ID            (developer.apple.com → Membership)
    3. App-specific password  (appleid.apple.com → Sign-In and Security
                               → App-Specific Passwords, format xxxx-xxxx-xxxx-xxxx)

EOF

    read -rp "  Apple ID: " NOTARY_APPLE_ID
    read -rp "  Team ID: " NOTARY_TEAM_ID
    read -rsp "  App-specific password: " NOTARY_PASSWORD
    echo

    if [ -z "$NOTARY_APPLE_ID" ] || [ -z "$NOTARY_TEAM_ID" ] || [ -z "$NOTARY_PASSWORD" ]; then
        echo "✗ All three fields are required." >&2
        exit 1
    fi

    echo "→ Validating with Apple and saving to Keychain as '$PROFILE_NAME'..."
    xcrun notarytool store-credentials "$PROFILE_NAME" \
        --apple-id "$NOTARY_APPLE_ID" \
        --team-id "$NOTARY_TEAM_ID" \
        --password "$NOTARY_PASSWORD"

    if [ ! -f .env.local ]; then
        echo "NOTARY_KEYCHAIN_PROFILE=\"$PROFILE_NAME\"" > .env.local
    elif ! grep -q "^NOTARY_KEYCHAIN_PROFILE=" .env.local; then
        echo "NOTARY_KEYCHAIN_PROFILE=\"$PROFILE_NAME\"" >> .env.local
    fi
    echo "✓ Saved. Future builds skip this step."
    echo

    NOTARY_KEYCHAIN_PROFILE="$PROFILE_NAME"
fi

# ─── Build ───────────────────────────────────────────────────────
#
# A universal build compiles every crate twice (arm64 + x86_64). With many
# parallel rustc jobs — especially alongside other apps / `cargo tauri dev`
# servers — peak memory can exhaust RAM and macOS kills the build mid-compile
# ("Terminated: 15", no error from cargo). Cap parallelism to keep peak memory
# sane; override by exporting CARGO_BUILD_JOBS yourself.
if [ -z "${CARGO_BUILD_JOBS:-}" ]; then
    CORES=$(sysctl -n hw.ncpu 2>/dev/null || echo 4)
    JOBS=$(( CORES / 2 )); [ "$JOBS" -lt 2 ] && JOBS=2
    export CARGO_BUILD_JOBS="$JOBS"
fi

# ─── LAN Drive extension (File Provider, Swift) ──────────────────

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "✗ xcodegen not found. Install: brew install xcodegen" >&2
    exit 1
fi
echo "→ Building LAN Drive extension..."
(
    cd app/macos
    xcodegen generate --quiet
    # Release builds arm64 + x86_64 (ONLY_ACTIVE_ARCH is off in Release).
    xcodebuild -quiet -project LANDrive.xcodeproj -target LANDrive -configuration Release \
        SYMROOT="$PWD/build" DEVELOPMENT_TEAM="$APPLE_TEAM_ID" CODE_SIGN_STYLE=Manual \
        CODE_SIGN_IDENTITY="$APPLE_SIGNING_IDENTITY" OTHER_CODE_SIGN_FLAGS=--timestamp build
)
# Keep the extension's entitlements (sandbox, App Group, network client) to re-sign it
# with after Tauri bundles the app.
EXT_ENTITLEMENTS="app/macos/build/LANDrive.entitlements.xml"
codesign -d --entitlements - --xml app/macos/build/Release/LANDrive.appex > "$EXT_ENTITLEMENTS"

# The app's entitlements: the same cs.* set as the other getapps.cafe apps, plus the App
# Group shared with the extension (bridge.json lives in its container).
APP_ENTITLEMENTS="app/macos/App.entitlements"
cat > "$APP_ENTITLEMENTS" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.cs.allow-jit</key>
    <true/>
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
    <true/>
    <key>com.apple.security.cs.disable-library-validation</key>
    <true/>
    <key>com.apple.security.application-groups</key>
    <array>
        <string>${APPLE_TEAM_ID}.cafe.getapps.networksharing</string>
    </array>
</dict>
</plist>
EOF

echo "→ Building... (CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS)"
# Notarizing happens below, after the re-sign; keep Tauri from notarizing a bundle we change.
(
    cd app
    env -u APPLE_ID -u APPLE_PASSWORD -u APPLE_API_KEY -u APPLE_API_ISSUER -u APPLE_API_KEY_PATH \
        cargo tauri build --config macos/ondemand.conf.json ${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}
)

DMG=$(find "$BUNDLE_BASE/dmg" -name "*.dmg" -print -quit 2>/dev/null || true)
if [ -z "$DMG" ]; then
    echo "✗ No .dmg produced. Check Tauri output above." >&2
    echo "  Hint: make sure bundle.active is true in tauri.conf.json." >&2
    exit 1
fi
echo "✓ Built: $DMG"

# ─── Locate built .app ───────────────────────────────────────────

APP=$(find "$BUNDLE_BASE/macos" -maxdepth 1 -name "*.app" -print -quit)
if [ -z "$APP" ]; then
    echo "✗ Built .app not found under $BUNDLE_BASE/macos" >&2
    exit 1
fi

# ─── Re-sign: extension with its own entitlements, then the app ──
#
# Tauri may sign nested code with the app's settings, dropping the extension's sandbox and
# App Group; macOS then won't load it. No --deep for the same reason. No provisioning
# profile: a Team-ID-prefixed App Group doesn't need one on macOS.

APPEX="$APP/Contents/PlugIns/LANDrive.appex"
if [ ! -d "$APPEX" ]; then
    echo "✗ LAN Drive extension missing from $APP" >&2
    exit 1
fi
echo "→ Re-signing LAN Drive extension + app"
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --options runtime --timestamp \
    --entitlements "$EXT_ENTITLEMENTS" "$APPEX"
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --options runtime --timestamp \
    --entitlements "$APP_ENTITLEMENTS" "$APP"
codesign --verify --strict --deep "$APP"
if ! codesign -d --entitlements - --xml "$APPEX" 2>/dev/null | grep -q "com.apple.security.app-sandbox"; then
    echo "✗ LAN Drive extension lost its sandbox entitlement" >&2
    exit 1
fi

# Already stapled? (Tauri may staple during build if APPLE_* env vars are set)
# We require BOTH .app and .dmg to be stapled — the .app staple is what
# prevents the "Apple cannot check for malicious software" warning when
# Launch Services opens a quarantined file with the installed app.
SKIP_NOTARIZE=0
if xcrun stapler validate "$APP" >/dev/null 2>&1 \
   && xcrun stapler validate "$DMG" >/dev/null 2>&1; then
    echo "✓ Already stapled (.app + .dmg) — skipping notarize."
    SKIP_NOTARIZE=1
fi

# ─── Notarize + staple ───────────────────────────────────────────

if [ "$SKIP_NOTARIZE" = "0" ]; then

NOTARY_ARGS=()
if [ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]; then
    echo "→ Notary auth: keychain profile '$NOTARY_KEYCHAIN_PROFILE'"
    NOTARY_ARGS=(--keychain-profile "$NOTARY_KEYCHAIN_PROFILE")
else
    echo "→ Notary auth: APPLE_ID env vars"
    NOTARY_ARGS=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
fi

# Staple the .app FIRST, then repackage the .dmg around the stapled .app.
# Without an .app-level staple, Gatekeeper has to re-verify online every
# time a quarantined file is opened with the app — combined with our JIT
# / unsigned-executable-memory entitlements that triggers a malware
# warning on second launch.
APP_ZIP_DIR=$(mktemp -d)
APP_ZIP="$APP_ZIP_DIR/app.zip"
echo "→ Zipping .app for notarization..."
ditto -c -k --keepParent "$APP" "$APP_ZIP"

echo "→ Submitting .app to Apple (1-3 min)..."
xcrun notarytool submit "$APP_ZIP" "${NOTARY_ARGS[@]}" --wait
rm -rf "$APP_ZIP_DIR"

echo "→ Stapling ticket onto .app..."
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"

echo "→ Repackaging .dmg with stapled .app"
STAGING=$(mktemp -d)
cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
rm -f "$DMG"
hdiutil create -volname "${PRODUCT:-App}" -srcfolder "$STAGING" \
    -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGING"

codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "$DMG"

echo "→ Submitting .dmg to Apple..."
xcrun notarytool submit "$DMG" "${NOTARY_ARGS[@]}" --wait

echo "→ Stapling ticket onto .dmg..."
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

echo
fi  # SKIP_NOTARIZE

echo "✓ Done — signed + notarized + stapled (.app + .dmg):"
echo "  $DMG"

# ─── Optional: publish to GitHub Release ─────────────────────────────────
# Wired so `build` ≡ `release` by default. Skip with SKIP_RELEASE=1, or
# delete this block if you only want signed/notarized DMGs locally.
if [ "${SKIP_RELEASE:-0}" = "0" ] \
   && command -v gh >/dev/null 2>&1 \
   && gh auth status >/dev/null 2>&1 \
   && [ -f scripts/release-mac.sh ]; then
    echo
    ./scripts/release-mac.sh --no-build
fi

