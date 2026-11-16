#!/bin/sh
# Usage: packaging/site.sh OUT_DIR [VERSION]
# VERSION defaults to the latest release.
set -eu
out=$1
version=${2:-$(gh release view --json tagName --jq .tagName)}
version=${version#v}
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
cp -R "$root/site/." "$out"
images=$root/fastlane/metadata/android/en-US/images
cp "$images"/*.png "$images"/phoneScreenshots/*.png "$out"
find "$out" -name '*.html' -exec sed -i "s/@VERSION@/$version/g" {} +
