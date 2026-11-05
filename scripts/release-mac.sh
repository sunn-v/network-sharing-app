#!/usr/bin/env bash
# Build → sign → notarize → publish a GitHub Release with the DMG attached.
# Idempotent: re-running for an existing tag updates the asset in-place.
#
# Usage:
#   ./scripts/release-mac.sh                                  # universal build, auto-notes
#   ./scripts/release-mac.sh --target aarch64-apple-darwin    # single arch
#   ./scripts/release-mac.sh --no-build                       # reuse last DMG
#   ./scripts/release-mac.sh --draft                          # publish as draft
#   ./scripts/release-mac.sh --notes "Custom release notes"
#
# Prereqs:
#   1. `gh` CLI installed + `gh auth login` done (Tauri-template repos
#      already use SSH, so a token via `gh auth` is the simplest auth path)
#   2. build-mac.sh prereqs (Developer ID cert + notary creds in Keychain)
#   3. Repo must be a GitHub remote (gh figures out owner/repo from origin)

set -euo pipefail

cd "$(dirname "$0")/.."

# ─── Parse args ───────────────────────────────────────────────────────────

NO_BUILD=""
DRAFT=""
CUSTOM_NOTES=""
TARGET_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) NO_BUILD=1; shift ;;
        --draft)    DRAFT="--draft"; shift ;;
        --notes)    shift; CUSTOM_NOTES="${1:-}"; shift ;;
        --notes=*)  CUSTOM_NOTES="${1#--notes=}"; shift ;;
        --target=*) TARGET_ARGS+=("$1"); shift ;;
        --target)   TARGET_ARGS+=("$1"); shift; TARGET_ARGS+=("${1:-}"); shift ;;
        *)
            echo "Unknown arg: $1" >&2
            exit 1 ;;
    esac
done

# Default to universal — matches the build invocation we already use.
if [ ${#TARGET_ARGS[@]} -eq 0 ]; then
    TARGET_ARGS=(--target universal-apple-darwin)
fi

# ─── Sanity: gh installed + authed ────────────────────────────────────────

if ! command -v gh >/dev/null 2>&1; then
    echo "✗ gh CLI not found. Install: brew install gh" >&2
    exit 1
fi
if ! gh auth status >/dev/null 2>&1; then
    echo "✗ gh not authenticated. Run: gh auth login" >&2
    exit 1
fi

# ─── Build (unless --no-build) ────────────────────────────────────────────

if [ -z "$NO_BUILD" ]; then
    ./scripts/build-mac.sh ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"}
fi

# ─── Locate the freshest .dmg ─────────────────────────────────────────────

DMG=$(find target -name "*.dmg" -type f -print0 2>/dev/null \
      | xargs -0 ls -t 2>/dev/null | head -1)

if [ -z "$DMG" ] || [ ! -f "$DMG" ]; then
    echo "✗ No .dmg under target. Did the build succeed?" >&2
    exit 1
fi
echo "→ Asset: $DMG"

# ─── Derive tag + title from tauri.conf.json ──────────────────────────────

VERSION=$(grep '"version"' app/tauri.conf.json | head -1 \
    | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
PRODUCT=$(grep '"productName"' app/tauri.conf.json | head -1 \
    | sed -E 's/.*"productName"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')

if [ -z "$VERSION" ] || [ -z "$PRODUCT" ]; then
    echo "✗ Couldn't read version / productName from tauri.conf.json" >&2
    exit 1
fi

TAG="v$VERSION"
TITLE="$PRODUCT $VERSION"

echo "→ Tag:   $TAG"
echo "→ Title: $TITLE"

# ─── Make sure tag exists on remote (else create + push) ─────────────────

if ! git rev-parse --verify --quiet "$TAG" >/dev/null; then
    echo "→ Tag $TAG not found locally — creating from HEAD"
    git tag -a "$TAG" -m "$TITLE"
fi
if ! git ls-remote --exit-code --tags origin "$TAG" >/dev/null 2>&1; then
    echo "→ Pushing tag $TAG to origin"
    git push origin "$TAG"
fi

# ─── Create release (or upload to existing) ──────────────────────────────

if gh release view "$TAG" >/dev/null 2>&1; then
    echo "→ Release $TAG already exists — overwriting asset"
    gh release upload "$TAG" "$DMG" --clobber
else
    echo "→ Creating release $TAG"
    # Default the notes to the tag commit's body (our "Bump X: …" commits carry
    # a clean bullet changelog) minus the Co-Authored-By trailer — much tidier
    # than --generate-notes dumping every commit. Falls back if the body is empty.
    if [ -z "$CUSTOM_NOTES" ]; then
        CUSTOM_NOTES=$(git log -1 --pretty=%b "$TAG^{commit}" 2>/dev/null \
            | sed '/^[Cc]o-[Aa]uthored-[Bb]y:/d')
    fi
    if [ -n "${CUSTOM_NOTES//[[:space:]]/}" ]; then
        gh release create "$TAG" "$DMG" --title "$TITLE" --notes "$CUSTOM_NOTES" $DRAFT
    else
        gh release create "$TAG" "$DMG" --title "$TITLE" --generate-notes $DRAFT
    fi
fi

echo
echo "✓ Released $TAG"
URL=$(gh release view "$TAG" --json url --jq .url)
echo "  $URL"
