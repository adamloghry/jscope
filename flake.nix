{
  description = "JournalScope, a terminal dashboard for the systemd journal";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/7dcdd87373d72e7a2173e4db02e20d3d7f495cfd";

  outputs =
    { self, nixpkgs, ... }:
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
            postInstall = ''
              install -Dm644 ${./LICENSE-MIT} "$out/share/licenses/jscope/LICENSE-MIT"
              install -Dm644 ${./LICENSE-APACHE} "$out/share/licenses/jscope/LICENSE-APACHE"
            '';
            meta = {
              mainProgram = "jscope";
              description = "Find which systemd service just got noisy";
              homepage = "https://16384.dev/journalscope";
              license = [ lib.licenses.mit lib.licenses.asl20 ];
            };
          };
          demo = pkgs.writeShellScriptBin "jscope-demo" ''
            exec ${lib.getExe binary} --no-defaults --input ${./fixtures/demo.jsonl} --mute-unit indexer.service "$@"
          '';
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
            vm-test = import ./nix/vm-test.nix { inherit pkgs binary; module = self.nixosModules.default; };
          };
          apps.default = {
            type = "app";
            program = lib.getExe binary;
            meta.description = "Watch the local systemd journal";
          };
          apps.demo = {
            type = "app";
            program = lib.getExe demo;
            meta.description = "Replay a synthetic queue spike in JournalScope";
          };
          # Re-take the README screenshots and the animated demo from a checkout's repository root.
          # It runs docs/screenshots/capture.py from the working tree, so edits to it need no rebuild.
          apps.screenshots = {
            type = "app";
            meta.description = "Re-capture docs/screenshots (README stills and demo.gif)";
            program = lib.getExe (
              pkgs.writeShellApplication {
                name = "jscope-screenshots";
                runtimeInputs = [
                  pkgs.xvfb-run
                  pkgs.xterm
                  pkgs.xdotool
                  pkgs.imagemagick
                  pkgs.ffmpeg-full # x11grab, which the default ffmpeg build lacks
                  pkgs.pngquant
                  pkgs.oxipng
                  pkgs.python3
                ];
                text = ''
                  if [ ! -f docs/screenshots/capture.py ]; then
                    echo "Run this from the root of a JournalScope checkout." >&2
                    exit 1
                  fi
                  FONTCONFIG_FILE=${pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; }}
                  JSCOPE_CAPTURE_FONT=${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSansMono.ttf
                  export FONTCONFIG_FILE JSCOPE_CAPTURE_FONT
                  exec xvfb-run -a -s "-screen 0 3200x2000x24 -nolisten tcp" \
                    python3 docs/screenshots/capture.py ${lib.getExe binary} "$@"
                '';
              }
            );
          };
          checks = {
            inherit tests;
            terminal = pkgs.runCommand "jscope-terminal"
              { nativeBuildInputs = [ (pkgs.python3.withPackages (ps: [ ps.pyte ])) ]; }
              ''
                python ${./tests/terminal.py} "${lib.getExe binary}" "${./fixtures/demo.jsonl}" "${./fixtures/generations.jsonl}" "${lib.getExe demo}"
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
    ) // {
      nixosModules.default = { pkgs, ... }: {
        imports = [
          ./nix/generation-markers.nix
          (import ./nix/program.nix {
            defaultPackage = perSystem.${pkgs.stdenv.hostPlatform.system}.packages.default;
          })
        ];
      };
    };
}
