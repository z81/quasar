#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root/web"

if ! command -v dx >/dev/null 2>&1; then
    echo 'Install Dioxus CLI first: cargo install dioxus-cli --version 0.7.10 --locked' >&2
    exit 1
fi

dx bundle --web --release --locked --debug-symbols false --out-dir dist

# ServeDir selects these when the browser accepts gzip; keep plain files too.
for asset in dist/public/assets/*; do
    case "$asset" in
        *.wasm|*.js|*.css) gzip -n -9 -c "$asset" > "$asset.gz" ;;
    esac
done
