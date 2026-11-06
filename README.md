# Litz Audio

Low-latency audio over LAN between a PC and an Android phone.

Use your phone as a microphone on Linux or Windows, and listen to your computer's audio on the phone. The computer runs a small background service, `litzd`; the Android app connects to it over your local network.

Litz is early software. Expect rough edges, especially on Windows.

[![Support Litz on Ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/arturoatomplay)

## Install

**Linux** (PipeWire 1.0 or newer):

```sh
curl -fsSL https://github.com/ArturoAtomplay/litz-audio/releases/latest/download/install.sh | sh
```

This installs Litz into `~/.local` and starts it at login. No root needed. There are also `.deb` and `.rpm` packages on the [releases page](https://github.com/ArturoAtomplay/litz-audio/releases), and this repository's flake provides a NixOS module (`services.litz-audio.enable = true`). With a package, start the service once:

```sh
systemctl --user enable --now litzd
```

**Windows**: run the installer from the [releases page](https://github.com/ArturoAtomplay/litz-audio/releases). To use the phone as a microphone you also need [VB-CABLE](https://vb-audio.com/Cable/).

**Android**: get it on [Google Play](https://play.google.com/store/apps/details?id=audio.litz), or install the APK from the releases page. Both are built from the same source; the Google Play version shows a small banner ad to fund development, and the APK has no ads.

## Usage

1. Connect the phone and computer to the same network.
2. Open Litz on the phone and tap your computer.
3. Click **Allow** on the computer.

That's it; the phone reconnects on its own from then on. In your calling app, pick **Litz Microphone** (Linux) or **CABLE Output** (Windows) as the microphone.

If the phone can't find the computer, use **Pair by IP address**, and make sure your firewall allows UDP port 47165 and mDNS (UDP 5353). USB tethering works too.

When a phone pairs, both screens show the same six-digit code; allow it only if they match. To see or remove paired phones on the computer, run `litzd devices` or `litzd forget NAME`. On Windows, run them from the install folder (`%LOCALAPPDATA%\Programs\Litz Audio`); the output appears after the prompt returns.

## Building

You need Rust (version pinned in `rust-toolchain.toml`), and on Linux the PipeWire headers, `pkg-config` and `libclang`. With Nix, `nix develop` provides everything.

```sh
packaging/fetch-model.sh target/model    # DeepFilterNet model, for tests
export LITZ_DATA_DIR=$PWD/target/model   # nix develop sets this for you
cargo build --release -p litzd
cargo test -p litz-core -p litzd
```

The Android app is in `android/` and needs JDK 17, the Android SDK and NDK, and `cargo-ndk` (`nix develop .#android` sets these up):

```sh
android/gradlew -p android :app:assembleFossDebug   # no ads, no proprietary libraries
android/gradlew -p android :app:assemblePlayDebug   # Google Play version, shows Google test ads
```

## Support

Litz is free and developed in spare time. If it is useful to you, a donation on [Ko-fi](https://ko-fi.com/arturoatomplay) helps cover devices for testing and keeps the project going. Bug reports and device test results help just as much.

## License

GPL-3.0-or-later, with an additional permission to link the Google Mobile Ads SDK in the Google Play version. See [LICENSE](LICENSE), [LICENSE-EXCEPTION](LICENSE-EXCEPTION) and [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES). Privacy policy: [PRIVACY.md](PRIVACY.md). To report a vulnerability, see [SECURITY.md](SECURITY.md). To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).
