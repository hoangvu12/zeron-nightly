#!/usr/bin/env bash
# The nightly's Linux engine, for Docker sandboxes: a chat placed in a
# sandbox runs `zeron-engine watchdog` there, which only the nightly's own
# engine has (patches/0019); the edge's releases are upstream's. The host's
# Install Zeron step fetches this tarball from the same release
# (ZERON_SANDBOX_ENGINE_RELEASES, patches/0024).
#
#   scripts/package-v3-linux-engine.sh <src>
#
# Writes <src>/target/package-v3/zeron-<version>-linux-<arch>.tar.gz, laid
# out like an edge release (one top folder, zeron-engine in it), so the
# step's `tar --strip-components=1` takes it as it takes those.
set -euo pipefail

src="$(cd "${1:?usage: package-v3-linux-engine.sh <src>}" && pwd)"
cd "$src"
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml | head -n1)"
[ -n "$version" ] || { echo "no [workspace.package] version in Cargo.toml" >&2; exit 1; }
arch="$(uname -m)"

cargo build --release --locked -p zeron --bin zeron-engine
said="$(./target/release/zeron-engine --version)"
[ "$said" = "zeron-engine $version" ] || { echo "zeron-engine says '$said', not 'zeron-engine $version'" >&2; exit 1; }
# Production's only, as on Windows: no staging edge or sign-in.
if grep -aqE 'staging\.zeron\.sh|zeron-edge-staging|client_01KWD0E9SYPV2WE4QN9TP9DP0W' target/release/zeron-engine; then
  echo "zeron-engine carries staging's edge or sign-in client" >&2
  exit 1
fi

out="$src/target/package-v3"
stage="$out/zeron-$version-linux-$arch"
rm -rf "$stage" "$stage.tar.gz"
mkdir -p "$stage"
install -m 755 target/release/zeron-engine "$stage/zeron-engine"
install -m 644 LICENSE THIRD_PARTY_NOTICES.md "$stage/"
tar -czf "$stage.tar.gz" -C "$out" "$(basename "$stage")"
rm -rf "$stage"
echo "packaged: $stage.tar.gz"
tar -tzvf "$stage.tar.gz"
