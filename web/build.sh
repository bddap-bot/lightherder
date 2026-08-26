#!/usr/bin/env bash
# Build the instrument for the browser into web/dist: the wasm module, the
# JavaScript that loads it, and the page that holds the canvas. That directory
# is what GitHub Pages publishes and what a local `python3 -m http.server`
# serves — there is no other web build.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
out=$here/dist

# The glue generator and the crate must be the same version to the patch: the
# tool refuses a module built against a different schema, and there is no
# version of this that is worth guessing at.
want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$root/Cargo.toml")
[ -n "$want" ] || { echo "Cargo.toml no longer pins wasm-bindgen exactly" >&2; exit 1; }
got=$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2 || true)
if [ "$got" != "$want" ]; then
  echo "wasm-bindgen $want required, found ${got:-none}." >&2
  echo "nix-shell brings the right one; outside it:" >&2
  echo "  cargo install --locked wasm-bindgen-cli --version $want" >&2
  exit 1
fi

cd "$root"
# Release: this is a feedback loop running every frame, and a debug wasm build
# is not worth looking at. `--lib` because the command line is a native thing
# — the page's entry point is the library's `web::start`.
cargo build --lib --release --target wasm32-unknown-unknown
rm -rf "$out"
wasm-bindgen --target web --no-typescript \
  --out-dir "$out" --out-name lightherder \
  "${CARGO_TARGET_DIR:-$root/target}/wasm32-unknown-unknown/release/lightherder.wasm"
cp "$here/index.html" "$out/index.html"
echo "web/dist: $(du -sh "$out" | cut -f1)"
