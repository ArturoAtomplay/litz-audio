#!/bin/sh
# Install Litz Audio for the current user on Linux. Needs PipeWire 1.x, no root.
#   curl -fsSL https://github.com/ArturoAtomplay/litz-audio/releases/latest/download/install.sh | sh
set -eu

repo=ArturoAtomplay/litz-audio

fail() {
  echo "litz: $*" >&2
  exit 1
}

[ "$(uname -s)" = Linux ] || fail "this script is for Linux; on Windows use the installer"
case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "unsupported architecture $(uname -m)" ;;
esac
for tool in curl tar sha256sum systemctl; do
  command -v "$tool" >/dev/null || fail "$tool is required"
done

tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
tag=${tag##*/}
case $tag in v*) ;; *) fail "no release found" ;; esac
name="litz-${tag#v}-linux-$arch"
url="https://github.com/$repo/releases/download/$tag"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "Downloading Litz ${tag#v}..."
curl -fsSL --retry 3 -o "$work/$name.tar.gz" "$url/$name.tar.gz"
curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$url/SHA256SUMS"
(cd "$work" && grep " $name.tar.gz\$" SHA256SUMS | sha256sum -c --quiet -) || fail "checksum mismatch"
tar -C "$work" -xzf "$work/$name.tar.gz"

systemctl --user stop litzd.service 2>/dev/null || true
cp -R "$work/$name/." "$HOME/.local/"
systemctl --user daemon-reload
systemctl --user enable --now litzd.service

echo "Litz is running and starts at sign-in."
echo "Allow UDP 47165 and mDNS (UDP 5353) through your firewall, then pair from the phone app."
