# Build environment for grapple on NixOS (or any Linux with Nix): `nix develop`, then `cd app/ui && pnpm app:build`.
# Node and pnpm are not pinned here (use your own, Node 20+ and pnpm 10+).
#
# `nix develop .#windows` is a second shell for type-checking the Windows build from Linux (no linking, no installer;
# the real Windows build runs in CI, .github/workflows/windows.yml):
#   nix develop .#windows -c cargo check --target x86_64-pc-windows-gnu -p grapple --tests
{
  description = "grapple: build environment";

  # The system's own nixpkgs (the registry entry), so the libraries match what NixOS already has.
  inputs.nixpkgs.url = "nixpkgs";
  # A Rust toolchain that carries the Windows standard library (nixpkgs' rustc only has the host's).
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ cargo rustc clippy rustfmt pkg-config ];
          buildInputs = with pkgs; [
            webkitgtk_4_1
            gtk3
            libsoup_3
            glib
            openssl
            dbus
            librsvg
            libayatana-appindicator
          ];
          # The tray icon library is loaded at run time (dlopen), not linked, so the linker would not add it to the
          # program's search path on its own.
          RUSTFLAGS = "-C link-arg=-Wl,-rpath,${pkgs.libayatana-appindicator}/lib";
          # GTK aborts when its GSettings schemas are missing, so a build started from this shell can find them.
          shellHook = ''
            export XDG_DATA_DIRS="${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
          '';
        };

        # Windows type-checking: Rust with the x86_64-pc-windows-gnu standard library, and the MinGW C compiler for
        # the build scripts that compile C (SQLite, aws-lc). `cargo check` only; nothing here links a .exe.
        windows =
          let
            rust = rust-overlay.lib.mkRustBin { } pkgs;
            # The same Rust release as the default shell (nixpkgs), so both checks see the same lints.
            toolchain = rust.stable.${pkgs.rustc.version}.minimal.override {
              extensions = [ "clippy" ];
              targets = [ "x86_64-pc-windows-gnu" ];
            };
            mingw = pkgs.pkgsCross.mingwW64.stdenv.cc;
            # nixpkgs' MinGW uses mcfgthreads; aws-lc wants the POSIX thread headers (sched.h) too.
            pthreads = pkgs.pkgsCross.mingwW64.windows.pthreads;
          in
          pkgs.mkShell {
            nativeBuildInputs = [ toolchain mingw pkgs.cmake pkgs.nasm pkgs.perl ];
            CFLAGS_x86_64_pc_windows_gnu = "-I${pthreads}/include";
            # Linking (optional, for running test binaries under Wine): Rust's MinGW std wants libpthread.a.
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${pthreads}/lib";
            CC_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-gcc";
            AR_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-ar";
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "x86_64-w64-mingw32-gcc";
          };
      });
    };
}
