{
  description = "Rust VS Code Dev Environment";
  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    fenix.url = "github:nix-community/fenix";
  };
  outputs = { self, nixpkgs, flake-utils, fenix, ...}:
    flake-utils.lib.eachDefaultSystem (
      system: 
      let
        pkgs = import nixpkgs {inherit system; config.allowUnfree = true; };
        rust-toolchain = fenix.packages.${system}.latest;
      in
      {
        devShells.default = with pkgs; mkShell rec {
          buildInputs = [
            (rust-toolchain.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
            ])
            libinput
            pkg-config
            systemd
            dbus
            cargo-udeps
            git
            udev udev.dev alsa-lib lutris
            vulkan-tools vulkan-headers vulkan-loader vulkan-validation-layers
            xorg.libX11 xorg.libXcursor xorg.libXi xorg.libXrandr # To use the x11 feature
            libxkbcommon wayland # To use the wayland feature
            rustc.llvmPackages.clang
            rustc.llvmPackages.bintools
            (wrapBintoolsWith { bintools = mold; })
          ];
          LIBCLANG_PATH = lib.makeLibraryPath [ rustc.llvmPackages.libclang.lib ];
          LD_LIBRARY_PATH = lib.makeLibraryPath buildInputs;
          RUST_SRC_PATH = "${rust-toolchain.rust-src}/lib/rustlib/src/rust/library";
          PATH = "${rust-toolchain.cargo}/bin";
          RUSTFLAGS = "-C link-arg=-fuse-ld=mold -C linker=clang -Zshare-generics=y";
        };
        packages.default = (pkgs.makeRustPlatform {
          cargo = rust-toolchain.toolchain;
          rustc = rust-toolchain.toolchain;
        }).buildRustPackage rec {
          pname = "trackpad-evdev-converter";
          version = "0.9.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.makeWrapper
          ];
          buildInputs = with pkgs; [
            libinput
            pkg-config
            systemd
            dbus
            cargo-udeps
            git
            udev udev.dev alsa-lib lutris
            vulkan-tools vulkan-headers vulkan-loader vulkan-validation-layers
            xorg.libX11 xorg.libXcursor xorg.libXi xorg.libXrandr # To use the x11 feature
            libxkbcommon wayland # To use the wayland feature
            rustc.llvmPackages.clang
            rustc.llvmPackages.bintools
            (wrapBintoolsWith { bintools = mold; })
          ];
          libraries = pkgs.lib.makeLibraryPath [pkgs.libinput pkgs.dbus];
          postInstall = ''
            mv $out/bin/trackpad-evdev-converter $out/bin/.trackpad-evdev-converter
            makeWrapper $out/bin/.trackpad-evdev-converter $out/bin/trackpad-evdev-converter --set LD_LIBRARY_PATH ${libraries}
            mkdir -p $out/share/dbus-1/system.d
            cp ${src}/dbus.conf $out/share/dbus-1/system.d/com.cowsociety.virtual_mouse.conf
          '';
        };
      }
    );
}