# Contributing to Litz Audio

Thanks for helping. Bug reports, fixes, device test results and translations are all welcome.

## Reporting bugs

Open an issue with the bug report form. Include:

- the Litz version on the phone and on the computer (`litzd --version`),
- the phone model and Android version, and the computer's OS (and PipeWire version on Linux),
- what you did, what you expected and what happened,
- the report from **Settings → About → Diagnostics** in the app, if you can open it.

Report security problems privately as described in [SECURITY.md](SECURITY.md), not in an issue.

## Making changes

1. For anything bigger than a small fix, open an issue first so we can agree on the approach.
2. Fork the repository and create a branch from `main`, such as `fix/usb-reconnect`.
3. Keep each pull request to one topic, and each commit to one logical change.
4. Run the checks below for the parts you touched.
5. Open a pull request that explains the user-visible change, lists the checks you ran, and links the issue. Include screenshots for UI changes. Call out any change to the wire protocol or pairing explicitly, since it affects compatibility between versions.

### Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) with a short imperative subject:

```
fix(android): reconnect after USB tethering restarts
feat(desktop): add a PipeWire output selector
```

Common types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `ci`, `build`. Release notes are generated from `feat`, `fix`, `perf` and `docs` subjects, so write them for users.

### Checks

With Nix, prefix each command with `nix develop --command` (or `nix develop .#android --command` for Android). See the README for setup without Nix.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
android/gradlew -p android checkKotlinFormat :app:testFossDebugUnitTest :app:testPlayDebugUnitTest :app:lintFossDebug :app:lintPlayDebug
```

Format Kotlin with `android/gradlew -p android formatKotlin`. If you change Android dependencies, refresh the lock and checksum files with `--write-locks --write-verification-metadata sha256` and commit them.

### Running a development build on NixOS

The NixOS firewall blocks phones from reaching a `litzd` you start by hand; the NixOS module opens the port only for the installed service. Open it once in your configuration:

```nix
networking.firewall.allowedUDPPorts = [ 47165 ];
```

Or open it until the next reboot without rebuilding, and close it again with `sudo nixos-firewall-tool reset`:

```sh
sudo nixos-firewall-tool open udp 47165
```

## Code layout

- `crates/core/`: protocol, media and transport shared by all platforms.
- `crates/desktop/`: the `litzd` daemon and audio backends.
- `crates/android-native/`: the Rust audio engine and Oboe adapter for Android.
- `android/app/src/main/`: the Android app. `src/foss/` and `src/play/` hold the only flavor-specific code: the Google Play flavor adds a banner ad and the consent form, and the foss flavor must never depend on proprietary libraries.

## License

By contributing, you agree that your contributions are licensed under the GNU General Public License version 3 or later, with the additional permission in [LICENSE-EXCEPTION](LICENSE-EXCEPTION).
