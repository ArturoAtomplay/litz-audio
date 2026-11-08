{
  description = "Litz Audio — background audio bridge";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      nativeBuildInputs = [
        pkgs.pkg-config
        pkgs.rustPlatform.bindgenHook
        pkgs.cmake
      ];
      buildInputs = [ pkgs.pipewire ];
      ndkVersion = "28.2.13676358";
      minimumApi = 23;
      # Same revision as the deep_filter git dependency; litzd loads its
      # DFN3 model at runtime from share/litz.
      deepFilterNet = pkgs.fetchFromGitHub {
        owner = "Rikorose";
        repo = "DeepFilterNet";
        rev = "d375b2d8309e0935d165700c91da9de862a99c31";
        sha256 = "sha256-JxvK5/PX29o23Pq9cNpTSyuAB7j9HszzDGHC2fOw74Y=";
      };
      modelDir = "${deepFilterNet}/models";
      androidTargets = {
        arm64-v8a = "aarch64-linux-android";
        x86_64 = "x86_64-linux-android";
      };
      androidPkgs = import nixpkgs {
        inherit system;
        config = {
          allowUnfree = true;
          android_sdk.accept_license = true;
        };
      };
      android = androidPkgs.androidenv.composeAndroidPackages {
        platformVersions = [ "37" ];
        buildToolsVersions = [ "36.0.0" ];
        includeNDK = true;
        ndkVersions = [ ndkVersion ];
        cmakeVersions = [ "3.22.1" ];
        includeEmulator = false;
      };
    in
    {
      packages.${system} = rec {
        litzd = pkgs.rustPlatform.buildRustPackage {
          pname = "litzd";
          buildType = "dist";
          version = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          src = self;
          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes."deep_filter-0.5.7-pre" = "sha256-JxvK5/PX29o23Pq9cNpTSyuAB7j9HszzDGHC2fOw74Y=";
          };
          cargoBuildFlags = [
            "-p"
            "litzd"
          ];
          cargoTestFlags = [
            "-p"
            "litzd"
          ];
          inherit nativeBuildInputs buildInputs;
          LITZ_DATA_DIR = modelDir;
          postInstall = ''
            install -Dm644 ${modelDir}/DeepFilterNet3_onnx.tar.gz $out/share/litz/DeepFilterNet3_onnx.tar.gz
            install -Dm644 ${./packaging/linux/litzd.service.in} $out/lib/systemd/user/litzd.service
            install -Dm644 ${./LICENSE} $out/share/licenses/litz/LICENSE
            install -Dm644 ${./THIRD_PARTY_NOTICES} $out/share/licenses/litz/THIRD_PARTY_NOTICES
            substituteInPlace $out/lib/systemd/user/litzd.service \
              --replace-fail '@BINDIR@' "$out/bin"
          '';
          meta.license = pkgs.lib.licenses.gpl3Plus;
        };
        default = litzd;
      };
      devShells.${system} = {
        windows = pkgs.mkShell {
          packages = [
            pkgs.rustup
            pkgs.cmake
            pkgs.ninja
            pkgs.pkgsCross.mingwW64.stdenv.cc
          ];
          buildInputs = [ pkgs.pkgsCross.mingwW64.windows.pthreads ];
          CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "x86_64-w64-mingw32-gcc";
          CC_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-gcc";
          CXX_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-g++";
        };
        default = pkgs.mkShell {
          packages = [
            pkgs.rustc
            pkgs.cargo
            pkgs.rustfmt
            pkgs.clippy
            pkgs.cargo-audit
            pkgs.pipewire
            pkgs.wireplumber
            pkgs.pulseaudio
            pkgs.ninja
          ];
          inherit nativeBuildInputs buildInputs;
          LITZ_DATA_DIR = modelDir;
        };
        android = pkgs.mkShell {
          packages = [
            android.androidsdk
            pkgs.jdk17
            pkgs.cargo-ndk
            pkgs.ninja
            pkgs.pkg-config
            pkgs.rustup
          ];
          LITZ_ANDROID_MIN_API = toString minimumApi;
          LITZ_ANDROID_NDK_VERSION = ndkVersion;
          LITZ_ANDROID_TARGETS = builtins.toJSON androidTargets;
          JAVA_HOME = "${pkgs.jdk17.home}";
          ANDROID_HOME = "${android.androidsdk}/libexec/android-sdk";
          ANDROID_NDK_HOME = "${android.androidsdk}/libexec/android-sdk/ndk/${ndkVersion}";
          # The SDK CMake 3.22.1 package bundles a Ninja binary that crashes on
          # NixOS; point AGP at the matching Nix-provided Ninja instead.
          LITZ_CMAKE_MAKE_PROGRAM = "${pkgs.ninja}/bin/ninja";
          GRADLE_OPTS = "-Dorg.gradle.project.android.aapt2FromMavenOverride=${android.androidsdk}/libexec/android-sdk/build-tools/36.0.0/aapt2";
        };
      };
      checks.${system}.package = self.packages.${system}.litzd;
      nixosModules.default = import ./nix/module.nix self;
    };
}
