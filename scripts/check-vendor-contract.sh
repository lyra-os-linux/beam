#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT

mkdir -p "$temporary/source/.cargo"
(
  cd "$repo_root"
  cargo vendor --locked --versioned-dirs "$temporary/source/vendor" >/dev/null
)
printf '%s\n' \
  '[source.crates-io]' \
  'replace-with = "vendored-sources"' \
  '[source.vendored-sources]' \
  'directory = "vendor"' >"$temporary/source/.cargo/config.toml"

tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
  -C "$temporary/source" -cf - .cargo vendor | zstd -q -19 -T1 -o "$temporary/vendor.tar.zst"
mkdir "$temporary/extracted"
tar --zstd -xf "$temporary/vendor.tar.zst" -C "$temporary/extracted"

if grep -F "$repo_root" "$temporary/extracted/.cargo/config.toml" >/dev/null; then
  echo "vendor config leaks the build-host path" >&2
  exit 1
fi
test -s "$temporary/vendor.tar.zst"
test -d "$temporary/extracted/vendor"
cp "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" "$temporary/extracted/"
cp -a "$repo_root/beam-core" "$repo_root/beam-gtk" "$temporary/extracted/"
(
  cd "$temporary/extracted"
  CARGO_NET_OFFLINE=true cargo metadata --locked --offline --no-deps --format-version 1 >/dev/null
)

echo "Beam vendor tarball is deterministic, relative and usable offline"
