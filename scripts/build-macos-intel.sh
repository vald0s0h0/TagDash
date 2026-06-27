#!/usr/bin/env bash
#
# Build (and optionally publish) the macOS Intel — darwin-x86_64 — bundle of TagDash
# on an Apple-Silicon Mac, NATIVELY under Rosetta 2.
#
# Why this exists
# ───────────────
# GitHub retired the Intel `macos-13` runners, so the release workflow now only
# builds Windows + macOS Apple-Silicon (arm64). x86_64 can NOT be cross-compiled on
# an arm64 host because whisper.cpp (CMake/Metal) and coreaudio-sys (bindgen) don't
# cross-compile cleanly — that's the original reason the CI went native-per-arch.
#
# The reliable workaround is to build x86_64 *natively* under Rosetta: when the whole
# toolchain (cargo, clang, cmake, bindgen) runs as x86_64, target == host == x86_64,
# so nothing is cross-compiled. This script re-execs itself under `arch -x86_64` and
# drives `tauri build --target x86_64-apple-darwin`.
#
# One-time setup on the Apple-Silicon Mac (everything x86_64, under Rosetta)
# ─────────────────────────────────────────────────────────────────────────
#   1. Rosetta:           softwareupdate --install-rosetta --agree-to-license
#   2. x86_64 Homebrew:   arch -x86_64 /bin/bash -c \
#                           "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
#                         (installs to /usr/local — distinct from the arm64 /opt/homebrew)
#   3. cmake + llvm:      arch -x86_64 /usr/local/bin/brew install cmake llvm
#   4. Rust:              install rustup if you don't have it (https://rustup.rs),
#                         then add the x86_64 toolchain (downloads native x86_64
#                         cargo/rustc — this is what makes the build native):
#                           rustup toolchain install stable-x86_64-apple-darwin
#   5. Node 20+ — must be x86_64 or UNIVERSAL (the whole build runs under Rosetta,
#                 so node runs as x86_64 too). The nodejs.org .pkg installer is
#                 universal; `brew install node` under the ARM Homebrew is NOT (it's
#                 arm64-only and fails under Rosetta with "Bad CPU type").
#
# Usage
# ─────
#   # 1) Build only (artifacts land in src-tauri/target/x86_64-apple-darwin/release/bundle):
#   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/tagdash.key)" \
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
#   ./scripts/build-macos-intel.sh
#
#   # 2) Build AND publish to an existing GitHub release (uploads the .dmg, the
#   #    updater .app.tar.gz + .sig, and merges the darwin-x86_64 entry into the
#   #    release's latest.json so Intel Macs keep auto-updating). Needs `gh` + `jq`:
#   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/tagdash.key)" \
#   ./scripts/build-macos-intel.sh --publish v0.1.5
#
set -euo pipefail

# ── Re-exec under Rosetta so the whole toolchain runs as x86_64 (native build) ──
if [ "$(uname -m)" != "x86_64" ]; then
  echo "→ Re-exec under Rosetta (arch -x86_64) so the build is native x86_64…"
  exec arch -x86_64 /bin/bash "$0" "$@"
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

PUBLISH_TAG=""
if [ "${1:-}" = "--publish" ]; then
  PUBLISH_TAG="${2:?--publish requires a release tag, e.g. --publish v0.1.5}"
fi

# ── Use the x86_64 toolchain's OWN cargo/rustc (native, not cross) ──────────────
# The rustup shim in ~/.cargo/bin is arm64 and can't run under Rosetta, so we put
# the x86_64 toolchain's real binaries first on PATH. Install it once with:
#   rustup toolchain install stable-x86_64-apple-darwin
X86_BIN="$HOME/.rustup/toolchains/stable-x86_64-apple-darwin/bin"
if [ ! -x "$X86_BIN/cargo" ]; then
  echo "✗ x86_64 Rust toolchain missing. Run once (in a normal terminal):" >&2
  echo "    rustup toolchain install stable-x86_64-apple-darwin" >&2
  exit 1
fi
export PATH="$X86_BIN:$PATH"
echo "→ Using x86_64 cargo: $X86_BIN/cargo"

# ── Build env ───────────────────────────────────────────────────────────────────
export MACOSX_DEPLOYMENT_TARGET=10.15            # whisper.cpp std::filesystem ≥ 10.15
# Prefer the x86_64 Homebrew (cmake/llvm) ONLY when it's actually installed (local
# Macs). On CI there is no Intel Homebrew — we rely on the universal cmake already on
# PATH (runs x86_64 under Rosetta) + the universal Xcode libclang — so DON'T prepend
# /usr/local/bin there (it could shadow the x86_64 cargo/node we put on PATH).
if [ -x /usr/local/bin/brew ]; then
  export PATH="/usr/local/bin:$PATH"
fi
# libclang for bindgen (coreaudio-sys / whisper-rs): use the x86_64 Homebrew LLVM when
# present; otherwise fall back to the universal Xcode libclang (bindgen finds it).
for d in /usr/local/opt/llvm/lib /usr/local/Cellar/llvm/*/lib; do
  [ -d "$d" ] && export LIBCLANG_PATH="$d" && break
done

if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  echo "⚠  TAURI_SIGNING_PRIVATE_KEY is not set — the updater .sig will not be produced."
  echo "    Set it to the contents of ~/.tauri/tagdash.key to enable auto-update for Intel."
fi
export TAURI_SIGNING_PRIVATE_KEY="${TAURI_SIGNING_PRIVATE_KEY:-}"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

echo "→ Installing frontend deps + building x86_64 bundle (native, under Rosetta)…"
npm ci
npm run tauri build -- --target x86_64-apple-darwin

BUNDLE="$REPO_ROOT/src-tauri/target/x86_64-apple-darwin/release/bundle"
DMG="$(ls "$BUNDLE"/dmg/*.dmg 2>/dev/null | head -1 || true)"
UPDATER="$(ls "$BUNDLE"/macos/*.app.tar.gz 2>/dev/null | head -1 || true)"
SIG="${UPDATER:+${UPDATER}.sig}"

echo ""
echo "✓ Build done:"
echo "    DMG     : ${DMG:-<none>}"
echo "    Updater : ${UPDATER:-<none>}"
echo "    Sig     : ${SIG:-<none (no signing key)>}"

if [ -z "$PUBLISH_TAG" ]; then
  cat <<EOF

Next (manual publish):
  • Upload the .dmg to the GitHub release:
      gh release upload <tag> "$DMG"
  • Add the Intel auto-update entry to latest.json (darwin-x86_64):
      {
        "signature": "<contents of ${SIG:-the .sig file}>",
        "url": "<the .app.tar.gz download URL on the release>"
      }
  • Or re-run with:  ./scripts/build-macos-intel.sh --publish <tag>
EOF
  exit 0
fi

# ── Auto-publish: upload artifacts + merge darwin-x86_64 into latest.json ───────
command -v gh >/dev/null 2>&1 || { echo "✗ gh (GitHub CLI) required for --publish" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "✗ jq required for --publish" >&2; exit 1; }
[ -n "$DMG" ] || { echo "✗ no .dmg produced — cannot publish" >&2; exit 1; }
[ -n "$UPDATER" ] && [ -f "$SIG" ] || { echo "✗ no signed updater bundle (.app.tar.gz.sig) — set TAURI_SIGNING_PRIVATE_KEY" >&2; exit 1; }

# Create the release if it doesn't exist yet (Intel may run before tauri-action).
if ! gh release view "$PUBLISH_TAG" &>/dev/null; then
  echo "→ Creating release ${PUBLISH_TAG} (first platform to publish)…"
  gh release create "$PUBLISH_TAG" --title "TagDash ${PUBLISH_TAG}" \
    --notes "Téléchargez l'installeur ci-dessous. La mise à jour automatique se fait au lancement de l'app."
fi

echo "→ Uploading .dmg + updater bundle to release ${PUBLISH_TAG}…"
gh release upload "$PUBLISH_TAG" "$DMG" "$UPDATER" --clobber

# The updater needs a stable download URL for the .app.tar.gz on this release.
UPDATER_NAME="$(basename "$UPDATER")"
REPO_SLUG="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
UPDATER_URL="https://github.com/${REPO_SLUG}/releases/download/${PUBLISH_TAG}/${UPDATER_NAME}"
SIG_CONTENT="$(cat "$SIG")"

echo "→ Merging darwin-x86_64 into latest.json…"
TMP="$(mktemp -d)"
# Download existing latest.json; if this is the first platform, seed an empty one.
if ! gh release download "$PUBLISH_TAG" --pattern latest.json --dir "$TMP" --clobber 2>/dev/null; then
  VERSION="${PUBLISH_TAG#v}"
  jq -n --arg ver "$VERSION" --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{ version: $ver, notes: "", pub_date: $date, platforms: {} }' > "$TMP/latest.json"
fi
jq --arg sig "$SIG_CONTENT" --arg url "$UPDATER_URL" \
   '.platforms."darwin-x86_64" = { "signature": $sig, "url": $url }' \
   "$TMP/latest.json" > "$TMP/latest.patched.json"
mv "$TMP/latest.patched.json" "$TMP/latest.json"
gh release upload "$PUBLISH_TAG" "$TMP/latest.json" --clobber
rm -rf "$TMP"

echo "✓ Intel artifacts published to $PUBLISH_TAG and latest.json now advertises darwin-x86_64."
