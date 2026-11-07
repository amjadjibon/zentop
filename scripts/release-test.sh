#!/usr/bin/env bash
# Tests for scripts/release.sh. Run: scripts/release-test.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELEASE="$HERE/release.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
failures=0

pass() { echo "ok - $1"; }
fail() { echo "not ok - $1"; failures=$((failures + 1)); }
expect_ok() { local name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }
expect_fail() {
  local name="$1" pattern="$2" output; shift 2
  if output=$("$@" 2>&1); then fail "$name (succeeded)"; elif [[ "$output" == *"$pattern"* ]]; then pass "$name"; else fail "$name ($output)"; fi
}

# A fixture project with a known version.
fixture() {
  local root="$WORK/$1"
  mkdir -p "$root"
  printf '[package]\nname = "zentop"\nversion = "%s"\n' "$2" >"$root/Cargo.toml"
  printf '[[package]]\nname = "zentop"\nversion = "%s"\n' "${3:-$2}" >"$root/Cargo.lock"
  echo "$root"
}

root=$(fixture same 1.4.0)
expect_ok "matching versions pass" env RELEASE_ROOT="$root" "$RELEASE" check-version v1.4.0
expect_fail "a different tag is rejected" "does not match" env RELEASE_ROOT="$root" "$RELEASE" check-version v9.0.0
expect_fail "lockfile drift is rejected" "Versions differ" env RELEASE_ROOT="$(fixture drift 1.4.0 1.3.0)" "$RELEASE" check-version
expect_fail "invalid versions are rejected" "Invalid release version" env RELEASE_ROOT="$(fixture bad 1.4)" "$RELEASE" check-version

# Windows staging picks the installer, never the portable binary.
packager="$WORK/packager"
mkdir -p "$packager"
echo installer >"$packager/Zentop_1.4.0_x64-setup.exe"
echo portable >"$packager/Zentop.exe"
if out=$(RELEASE_ROOT="$root" "$RELEASE" stage windows-x86_64 "$packager" "$WORK/staged") &&
  [[ "$out" == "Zentop_1.4.0_x64-setup.exe" && "$(cat "$WORK/staged/$out")" == installer ]]; then
  pass "Windows staging selects the installer"
else
  fail "Windows staging selects the installer"
fi
echo stale >"$packager/stale_x64-setup.exe"
expect_fail "ambiguous packages are rejected" "found 2" env RELEASE_ROOT="$root" "$RELEASE" stage windows-x86_64 "$packager" "$WORK/staged2"

# The manifest requires every platform and records verifiable hashes.
artifacts="$WORK/artifacts"
mkdir -p "$artifacts"
for name in Zentop_1.4.0_x86_64.AppImage Zentop_1.4.0_x86_64.deb Zentop_1.4.0_x86_64.rpm \
  Zentop_1.4.0_aarch64.AppImage Zentop_1.4.0_aarch64.deb Zentop_1.4.0_aarch64.rpm \
  intel-Zentop_1.4.0_x64.dmg silicon-Zentop_1.4.0_aarch64.dmg Zentop_1.4.0_x64-setup.exe; do
  echo "package $name" >"$artifacts/$name"
done
expect_ok "a complete set produces a manifest" env RELEASE_ROOT="$root" GITHUB_SHA=abc "$RELEASE" manifest "$artifacts" v1.4.0
if (cd "$artifacts" && { command -v sha256sum >/dev/null && sha256sum -c SHA256SUMS || shasum -a 256 -c SHA256SUMS; }) >/dev/null 2>&1; then
  pass "checksums verify independently"
else
  fail "checksums verify independently"
fi
if grep -q '"commit": "abc"' "$artifacts/release-manifest.json" && [[ $(grep -o '"sha256"' "$artifacts/release-manifest.json" | wc -l) -eq 9 ]]; then
  pass "manifest lists all nine packages and the commit"
else
  fail "manifest lists all nine packages and the commit"
fi
echo stale >"$artifacts/Zentop_old_version.exe"
expect_fail "unexpected packages are rejected" "Unexpected: Zentop_old_version.exe" env RELEASE_ROOT="$root" "$RELEASE" manifest "$artifacts" v1.4.0
rm "$artifacts/Zentop_old_version.exe" "$artifacts/intel-Zentop_1.4.0_x64.dmg"
expect_fail "missing packages are rejected" "Missing: intel-Zentop_1.4.0_x64.dmg" env RELEASE_ROOT="$root" "$RELEASE" manifest "$artifacts" v1.4.0

# The real project is internally consistent.
expect_ok "repository versions agree" "$RELEASE" check-version

if [[ $failures -gt 0 ]]; then
  echo "$failures test(s) failed"
  exit 1
fi
echo "all release tests passed"
