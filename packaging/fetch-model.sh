#!/bin/sh
# Download the DeepFilterNet3 model that litzd loads at runtime and verify it.
# Usage: packaging/fetch-model.sh DIR
set -eu
rev=d375b2d8309e0935d165700c91da9de862a99c31
sha=c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616
dir=${1:?usage: fetch-model.sh DIR}
file="$dir/DeepFilterNet3_onnx.tar.gz"
mkdir -p "$dir"
if [ ! -f "$file" ]; then
  curl -fsSL --retry 3 -o "$file.part" \
    "https://raw.githubusercontent.com/Rikorose/DeepFilterNet/$rev/models/DeepFilterNet3_onnx.tar.gz"
  mv "$file.part" "$file"
fi
echo "$sha  $file" | sha256sum -c -
