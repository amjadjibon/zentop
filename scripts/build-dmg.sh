#!/usr/bin/env bash
# Build a macOS .dmg locally.
#
#   scripts/build-dmg.sh            # this Mac's architecture
#   scripts/build-dmg.sh aarch64    # Apple silicon
#   scripts/build-dmg.sh x86_64     # Intel (cross-compiles on Apple silicon)
#
# cargo-packager builds Zentop.app from the [package.metadata.packager] config.
# With APPLE_SIGNING_IDENTITY and the other APPLE_* signing credentials
# exported, it signs and notarizes the app; otherwise the app is ad-hoc signed,
# which runs on this Mac and opens elsewhere after right-click → Open.
# The DMG is made with hdiutil, so no Finder automation permission is needed,
# and lands in artifacts/ under its release name.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGER_VERSION="0.11.8"

die() {
  echo "$*" >&2
  exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || die "DMGs can only be built on macOS."

arch="${1:-$(uname -m)}"
case "$arch" in
  aarch64 | arm64) target="aarch64-apple-darwin" platform="macos-aarch64" ;;
  x86_64 | intel) target="x86_64-apple-darwin" platform="macos-x86_64" ;;
  *) die "Unknown architecture: $arch (use aarch64 or x86_64)" ;;
esac

if ! cargo packager --version 2>/dev/null | grep -q "$PACKAGER_VERSION"; then
  echo "Installing cargo-packager $PACKAGER_VERSION..."
  cargo install --locked "cargo-packager@$PACKAGER_VERSION"
fi
if command -v rustup >/dev/null; then
  rustup target add "$target" >/dev/null
fi

cd "$ROOT"
rm -rf target/packager
cargo build --release --locked --target "$target"
export APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:--}"
cargo packager --release --target "$target" --formats app
app=target/packager/Zentop.app
if [[ "$APPLE_SIGNING_IDENTITY" == "-" ]]; then
  # cargo-packager's ad-hoc signature leaves the bundle unsealed, which macOS
  # reports as "damaged" after download. Seal the whole bundle instead.
  codesign --force --deep --sign - "$app"
fi
codesign --verify --deep --strict "$app" || die "Zentop.app is not validly signed."

staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname Zentop -srcfolder "$staging" -fs HFS+ -format UDZO \
  -ov target/packager/Zentop.dmg

name=$(scripts/release.sh stage "$platform" target/packager artifacts)
dmg="$ROOT/artifacts/$name"
hdiutil verify -quiet "$dmg" || die "The DMG failed verification: $dmg"
echo "Built $dmg"
