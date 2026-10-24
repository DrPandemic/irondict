#!/usr/bin/env bash
# Fail unless the bundled GCIDE `.dict.dz` inside an APK is stored
# uncompressed. irondict memory-maps that file in place from `base.apk`, which
# only works when it is `Stored`; a compressed asset would force a 34 MB copy
# into RAM (the Android entry point refuses the compressed path and skips GCIDE).
#
# `aapt` currently stores it on its own (deflating an already-gzipped file
# doesn't shrink it), but this guards against a future change in `aapt` or in
# cargo-apk's packaging. Run it after `cargo apk build` in CI/release scripts.
#
# Usage: crates/android/verify-apk-assets.sh <path-to.apk>
set -euo pipefail

apk="${1:?usage: verify-apk-assets.sh <apk>}"

row="$(unzip -v "$apk" | awk '$NF ~ /assets\/gcide\/.*\.dict\.dz$/ { print; found = 1 } END { exit !found }')" || {
    echo "error: assets/gcide/*.dict.dz not found in $apk" >&2
    exit 1
}
method="$(awk '{ print $2 }' <<<"$row")"
if [ "$method" != "Stored" ]; then
    echo "error: bundled GCIDE .dict.dz is compressed in $apk:" >&2
    echo "       $row" >&2
    echo "       It must be Stored so irondict can map it in place." >&2
    exit 1
fi
echo "ok: GCIDE .dict.dz is stored: $row"