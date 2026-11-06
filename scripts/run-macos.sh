#!/bin/sh
# Local development wrapper, not an installer or a distribution/signing pipeline.
set -eu
if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'This launcher is for macOS.' >&2
    exit 1
fi
case "${1:-}" in
    "") launch=yes ;;
    --build-only) launch=no ;;
    *) printf '%s\n' 'Usage: sh scripts/run-macos.sh [--build-only]' >&2; exit 1 ;;
esac
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
target_dir=${CARGO_TARGET_DIR:-target}
cargo build -p tgbooks-app --locked --target-dir "$target_dir"
binary_dir=$target_dir
if [ -n "${CARGO_BUILD_TARGET:-}" ]; then
    binary_dir=$binary_dir/$CARGO_BUILD_TARGET
fi
app="$root/target/dev-bundle/Telegram Books.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary_dir/debug/tgbooks" "$app/Contents/MacOS/tgbooks"
cp crates/app/resources/macos/Info.plist "$app/Contents/Info.plist"
cp crates/app/resources/branding/app-icon.icns "$app/Contents/Resources/app-icon.icns"
codesign --force --sign - "$app"
printf 'Built %s\n' "$app"
if [ "$launch" = yes ]; then
    if [ -n "${TGBOOKS_DATA_DIR:-}" ]; then
        open --env "TGBOOKS_DATA_DIR=$TGBOOKS_DATA_DIR" "$app"
    else
        open "$app"
    fi
fi
