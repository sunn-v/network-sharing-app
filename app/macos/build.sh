#!/usr/bin/env bash
# Build Network Sharing for macOS with on-demand LAN Drive (File Provider extension).
#
# Needs: Xcode, xcodegen (brew install xcodegen), Rust, Node.
# Env:   APPLE_TEAM_ID           your 10-character team id
#        APPLE_SIGNING_IDENTITY  e.g. "Developer ID Application: Name (TEAMID)"
# Extra arguments go to `tauri build` (e.g. --target aarch64-apple-darwin).
# Notarize the resulting .dmg the usual way afterwards (xcrun notarytool).
set -euo pipefail
cd "$(dirname "$0")"
: "${APPLE_TEAM_ID:?set APPLE_TEAM_ID}"
: "${APPLE_SIGNING_IDENTITY:?set APPLE_SIGNING_IDENTITY}"

# 1. The extension (.appex), signed with the same identity as the app.
xcodegen generate --quiet
xcodebuild -quiet -project LANDrive.xcodeproj -target LANDrive -configuration Release \
  SYMROOT="$PWD/build" DEVELOPMENT_TEAM="$APPLE_TEAM_ID" CODE_SIGN_STYLE=Manual \
  CODE_SIGN_IDENTITY="$APPLE_SIGNING_IDENTITY" OTHER_CODE_SIGN_FLAGS=--timestamp build

# 2. The app's entitlements: the same App Group, so both can reach bridge.json.
cat > App.entitlements <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.application-groups</key>
  <array>
    <string>${APPLE_TEAM_ID}.cafe.getapps.networksharing</string>
  </array>
</dict>
</plist>
EOF

# 3. The app, with the extension copied into Contents/PlugIns. APPLE_TEAM_ID is read at compile
#    time to switch LAN Drive to on-demand.
cd ..
export APPLE_TEAM_ID APPLE_SIGNING_IDENTITY
npx -y @tauri-apps/cli@2 build --config macos/ondemand.conf.json "$@"
