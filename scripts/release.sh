#!/usr/bin/env bash
# Release checks and packaging helpers.
#
#   release.sh check-version [tag]
#   release.sh stage <platform> <packager-dir> <output-dir>
#   release.sh check-appimage <packager-dir>
#   release.sh manifest <artifact-dir> <tag>
#   release.sh upload-draft <artifact-dir> <tag>     (needs gh and GITHUB_SHA)
set -euo pipefail

ROOT="${RELEASE_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

die() {
  echo "$*" >&2
  exit 1
}

# platform -> "extension:published-name" pairs. Keep download names stable for
# package managers and the existing release history.
packages_for() {
  case "$1" in
    linux-x86_64)
      echo ".AppImage:Zentop_VERSION_x86_64.AppImage .deb:Zentop_VERSION_x86_64.deb .rpm:Zentop_VERSION_x86_64.rpm" ;;
    linux-aarch64)
      echo ".AppImage:Zentop_VERSION_aarch64.AppImage .deb:Zentop_VERSION_aarch64.deb .rpm:Zentop_VERSION_aarch64.rpm" ;;
    macos-x86_64) echo ".dmg:intel-Zentop_VERSION_x64.dmg" ;;
    macos-aarch64) echo ".dmg:silicon-Zentop_VERSION_aarch64.dmg" ;;
    # An installer, not the portable binary.
    windows-x86_64) echo "-setup.exe:Zentop_VERSION_x64-setup.exe" ;;
    *) return 1 ;;
  esac
}
PLATFORMS="linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64 windows-x86_64"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

file_size() {
  wc -c <"$1" | tr -d ' '
}

check_version() {
  local tag="${1:-}" cargo lock
  cargo=$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{gsub(/.*= *"|".*/,"");print;exit}' "$ROOT/Cargo.toml")
  lock=$(awk '/^name = "zentop"$/{getline; gsub(/version = "|"/,""); print; exit}' "$ROOT/Cargo.lock")
  [[ -n "$cargo" && "$cargo" == "$lock" ]] || die "Versions differ (Cargo.toml, Cargo.lock): ${cargo:-?}, ${lock:-?}"
  [[ "$cargo" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]] ||
    die "Invalid release version: $cargo"
  [[ -z "$tag" || "$tag" == "v$cargo" ]] || die "Tag $tag does not match v$cargo"
  echo "$cargo"
}

expected_assets() {
  local version="$1" platform pair
  for platform in $PLATFORMS; do
    for pair in $(packages_for "$platform"); do
      echo "${pair#*:}" | sed "s/VERSION/$version/"
    done
  done | LC_ALL=C sort
}

stage() {
  local platform="$1" source="$2" destination="$3" version pairs pair extension name matches
  pairs=$(packages_for "$platform") || die "Unknown platform: $platform"
  version=$(check_version)
  mkdir -p "$destination"
  local staged=()
  for pair in $pairs; do
    extension="${pair%%:*}"
    name=$(echo "${pair#*:}" | sed "s/VERSION/$version/")
    matches=()
    for file in "$source"/*"$extension"; do
      [[ -f "$file" ]] && matches+=("$file")
    done
    [[ ${#matches[@]} -eq 1 ]] || die "Expected one $extension package in $source, found ${#matches[@]}"
    [[ -s "${matches[0]}" ]] || die "Empty package: ${matches[0]}"
    staged+=("${matches[0]}:$name")
  done
  for pair in "${staged[@]}"; do
    cp "${pair%%:*}" "$destination/${pair#*:}"
    echo "${pair#*:}"
  done
}

check_appimage() {
  local directory="$1" images=() temporary found
  for file in "$directory"/*.AppImage; do
    [[ -f "$file" ]] && images+=("$file")
  done
  [[ ${#images[@]} -eq 1 ]] || die "Expected one AppImage, found ${#images[@]}"
  temporary=$(mktemp -d)
  trap 'rm -rf "$temporary"' RETURN
  local image
  image="$(cd "$(dirname "${images[0]}")" && pwd)/$(basename "${images[0]}")"
  (cd "$temporary" && "$image" --appimage-extract >/dev/null) || die "Cannot inspect AppImage"
  # Do not follow symlinks into the build host's library directories (see #203).
  found=$(find -P "$temporary/squashfs-root" -name 'libwayland-client.so*' | xargs -r -n1 basename)
  [[ -z "$found" ]] || die "AppImage bundles host Wayland libraries (see #203): $found"
  echo "AppImage uses the host Wayland client library."
}

manifest() {
  local directory="$1" tag="$2" version expected actual commit="${GITHUB_SHA:-}"
  version=$(check_version "$tag")
  expected=$(expected_assets "$version")
  actual=$(cd "$directory" && find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name release-manifest.json |
    sed 's|^\./||' | LC_ALL=C sort)
  if [[ "$expected" != "$actual" ]]; then
    die "$(printf 'Release packages are incomplete or unexpected.\nMissing: %s\nUnexpected: %s' \
      "$(comm -23 <(echo "$expected") <(echo "$actual") | paste -sd, -)" \
      "$(comm -13 <(echo "$expected") <(echo "$actual") | paste -sd, -)")"
  fi
  : >"$directory/SHA256SUMS"
  local assets="" name path size hash
  while read -r name; do
    path="$directory/$name"
    [[ -s "$path" ]] || die "Missing or empty package: $path"
    hash=$(sha256 "$path")
    size=$(file_size "$path")
    printf '%s  %s\n' "$hash" "$name" >>"$directory/SHA256SUMS"
    assets+="{\"name\":\"$name\",\"size\":$size,\"sha256\":\"$hash\"},"
  done <<<"$expected"
  local commit_json="null"
  [[ -n "$commit" ]] && commit_json="\"$commit\""
  printf '{\n  "schemaVersion": 1,\n  "version": "%s",\n  "commit": %s,\n  "assets": [%s]\n}\n' \
    "$version" "$commit_json" "${assets%,}" >"$directory/release-manifest.json"
  cat "$directory/release-manifest.json"
}

upload_draft() {
  local directory="$1" tag="$2" version state names
  [[ -n "${GITHUB_SHA:-}" ]] || die "GITHUB_SHA is required."
  version=$(check_version "$tag")
  # Validate everything before making any GitHub writes.
  manifest "$directory" "$tag" >/dev/null
  [[ "$(git rev-list -n1 "$tag")" == "$GITHUB_SHA" ]] ||
    die "The version tag no longer points to the commit built by this workflow."
  names=$(expected_assets "$version"; printf 'SHA256SUMS\nrelease-manifest.json\n')
  if state=$(gh release view "$tag" --json isDraft,assets 2>/dev/null); then
    [[ "$(echo "$state" | jq -r .isDraft)" == "true" ]] ||
      die "Refusing to modify an already published release."
    local unexpected
    unexpected=$(echo "$state" | jq -r '.assets[].name' | grep -vxF -f <(echo "$names") || true)
    [[ -z "$unexpected" ]] || die "The existing draft has unexpected assets: $unexpected"
  else
    local prerelease=()
    [[ "$version" == *-* ]] && prerelease=(--prerelease)
    gh release create "$tag" --draft --verify-tag --target "$GITHUB_SHA" \
      --title "Zentop v$version" "${prerelease[@]}" \
      --notes "Built from $GITHUB_SHA.

All five platform builds passed. Checksums and a build manifest are attached.

Review the packages and release notes before publishing."
  fi
  local files=()
  while read -r name; do files+=("$directory/$name"); done <<<"$names"
  gh release upload "$tag" "${files[@]}" --clobber
}

command="${1:-}"
shift || true
case "$command" in
  check-version)
    version=$(check_version "${1:-}")
    echo "Release version: $version"
    ;;
  stage) stage "$@" ;;
  check-appimage) check_appimage "$@" ;;
  manifest) manifest "$@" ;;
  upload-draft) upload_draft "$@" ;;
  *) die "Usage: release.sh check-version [tag] | stage <platform> <dir> <out> | check-appimage <dir> | manifest <dir> <tag> | upload-draft <dir> <tag>" ;;
esac
