#!/usr/bin/env bash
# Builds a SOAT release on this machine, without CI: binaries, checksums
# and a container image. See docs/releases.md.
#
#   scripts/release.sh              # binaries for this machine (+ Linux via Docker), image
#   scripts/release.sh --no-image   # binaries only
#   EXTRA_CA=/path/ca.pem scripts/release.sh   # behind a TLS-inspecting proxy
#
# Output: dist/<version>/soat-<version>-<target>.tar.gz, SHA256SUMS, and
# the image soat:<version> (and soat:latest).
set -euo pipefail

cd "$(dirname "$0")/.."
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
out="dist/$version"
image=true
for arg in "$@"; do
  case "$arg" in
    --no-image) image=false ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
mkdir -p "$out"

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "the working tree has changes: commit them first, so the release is what the commit says" >&2
  exit 1
fi
commit=$(git rev-parse --short HEAD)

# One archive per target: the binary, the license and the readme.
package() {
  local target=$1 bin=$2
  local name="soat-$version-$target"
  local dir
  dir=$(mktemp -d)
  mkdir "$dir/$name"
  cp "$bin" "$dir/$name/soat"
  cp LICENSE README.md "$dir/$name/"
  tar -C "$dir" -czf "$out/$name.tar.gz" "$name"
  rm -rf "$dir"
  echo "built $out/$name.tar.gz"
}

# Linux, in Debian 12's container so the binary runs on glibc 2.36 and
# newer, not only on this machine's.
linux() {
  local platform=$1 target=$2
  local ca_args=()
  if [ -n "${EXTRA_CA:-}" ]; then
    ca_args=(-v "$EXTRA_CA:/etc/ssl/certs/extra-ca.pem:ro"
      -e SSL_CERT_FILE=/etc/ssl/certs/extra-ca.pem
      -e CARGO_HTTP_CAINFO=/etc/ssl/certs/extra-ca.pem)
  fi
  docker run --rm --platform "$platform" --network host \
    -e HTTPS_PROXY -e HTTP_PROXY -e NO_PROXY -e https_proxy -e http_proxy -e no_proxy \
    "${ca_args[@]}" \
    -v "$PWD:/src" -v soat-cargo-registry:/usr/local/cargo/registry \
    -v soat-cargo-git:/usr/local/cargo/git -w /src \
    rust:1.100-bookworm \
    cargo build --release --locked -p soat --target-dir "target/release-$target"
  package "$target" "target/release-$target/release/soat"
}

case "$(uname -s)" in
  Darwin)
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
      rustup target add "$target" >/dev/null
      cargo build --release --locked -p soat --target "$target"
      package "$target" "target/$target/release/soat"
    done
    ;;
esac

if command -v docker >/dev/null; then
  linux linux/amd64 x86_64-unknown-linux-gnu
  # Another architecture runs under emulation (binfmt/QEMU): slow, opt in.
  if [ -n "${ARM64:-}" ]; then
    linux linux/arm64 aarch64-unknown-linux-gnu
  fi
elif [ "$(uname -s)" = Linux ]; then
  cargo build --release --locked -p soat
  package "$(uname -m)-unknown-linux-gnu" target/release/soat
fi

(cd "$out" && sha256sum ./*.tar.gz 2>/dev/null || shasum -a 256 ./*.tar.gz) > "$out/SHA256SUMS.tmp"
mv "$out/SHA256SUMS.tmp" "$out/SHA256SUMS"
echo "checksums in $out/SHA256SUMS"

if $image && command -v docker >/dev/null; then
  docker build --platform linux/amd64 -f Dockerfile \
    --build-arg BINARY="target/release-x86_64-unknown-linux-gnu/release/soat" \
    --label "org.opencontainers.image.version=$version" \
    --label "org.opencontainers.image.revision=$commit" \
    -t "soat:$version" -t soat:latest .
  docker save "soat:$version" | gzip > "$out/soat-$version-image-amd64.tar.gz"
  echo "built image soat:$version (also saved as $out/soat-$version-image-amd64.tar.gz)"
fi
