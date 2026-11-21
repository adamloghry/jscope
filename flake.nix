{
  description = "JournalScope, a terminal dashboard for the systemd journal";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/7dcdd87373d72e7a2173e4db02e20d3d7f495cfd";

  outputs =
    { nixpkgs, ... }:
    let
      inherit (nixpkgs) lib;
      forLinux = lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
      perSystem = forLinux (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests
              ./fixtures
            ];
          };
          hegelEngine = pkgs.rustPlatform.buildRustPackage {
            pname = "jscope-hegel-engine";
            version = "0.51.6";
            src = ./nix/hegel-engine;
            cargoLock.lockFile = ./nix/hegel-engine/Cargo.lock;
            cargoBuildFlags = [ "--lib" ];
            doCheck = false;
            installPhase = ''
              runHook preInstall
              install -Dm755 "target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/libhegel_c.so" "$out/lib/libhegel_c.so"
              runHook postInstall
            '';
          };
          binary = pkgs.rustPlatform.buildRustPackage {
            pname = "jscope";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "--bin" "jscope" ];
            doCheck = false;
            meta.mainProgram = "jscope";
          };
          tests = binary.overrideAttrs (attrs: {
            pname = "jscope-tests";
            doCheck = true;
            nativeBuildInputs = attrs.nativeBuildInputs ++ [ pkgs.clippy ];
            HEGEL_C_LIB_DIR = "${hegelEngine}/lib";
            cargoTestFlags = [ "--locked" ];
            postCheck = ''
              cargo clippy --offline --locked --all-targets -- -D warnings
            '';
          });
        in
        {
          packages = {
            default = binary;
            vm-test = import ./nix/vm-test.nix { inherit pkgs binary; };
          };
          apps.default = {
            type = "app";
            program = lib.getExe binary;
            meta.description = "Watch the local systemd journal";
          };
          checks = {
            inherit tests;
            terminal = pkgs.runCommand "jscope-terminal"
              { nativeBuildInputs = [ (pkgs.python3.withPackages (ps: [ ps.pyte ])) ]; }
              ''
                python ${./tests/terminal.py} "${lib.getExe binary}" "${./fixtures/demo.jsonl}"
                touch "$out"
              '';
            fmt = pkgs.runCommand "jscope-fmt"
              { nativeBuildInputs = [ pkgs.cargo pkgs.rustfmt ]; }
              ''
                cd "${src}"
                cargo fmt --all --check
                rustfmt --edition 2024 --check "${./nix/hegel-engine/src/lib.rs}"
                touch "$out"
              '';
          };
          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.jujutsu
              pkgs.rustc
              pkgs.rustfmt
            ];
            HEGEL_C_LIB_DIR = "${hegelEngine}/lib";
          };
        }
      );
    in
    lib.genAttrs [ "packages" "apps" "checks" "devShells" ] (
      output: forLinux (system: perSystem.${system}.${output})
    );
}
