{inputs, ...}: {
  perSystem = {
    lib,
    pkgs,
    system,
    ...
  }: let
    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
      extensions = ["llvm-tools-preview"];
    };
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../.config
        ../Cargo.lock
        ../Cargo.toml
        ../crates
        ../deny.toml
      ];
    };

    generatedCargoNix = inputs.crate2nix.tools.${system}.generatedCargoNix {
      name = "kraai-checks";
      inherit src;
    };

    cargoNix = pkgs.callPackage generatedCargoNix {
      buildRustCrateForPkgs = pkgs:
        pkgs.buildRustCrate.override {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
    };

    mkCargoCheck = {
      name,
      command,
      nativeBuildInputs ? [],
      buildInputs ? [],
      env ? {},
    }:
      pkgs.stdenv.mkDerivation {
        pname = name;
        version = "0.0.0";
        inherit src;
        strictDeps = true;
        nativeBuildInputs =
          [
            rustToolchain
            pkgs.pkg-config
          ]
          ++ nativeBuildInputs;
        buildInputs =
          [
            pkgs.openssl
          ]
          ++ buildInputs;
        buildPhase = let
          exportEnv = lib.concatLines (
            lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg value}") env
          );
        in ''
          export HOME="$TMPDIR/home"
          mkdir -p "$HOME"
          export CARGO_HOME="$TMPDIR/cargo-home"
          mkdir -p "$CARGO_HOME"
          cp "${generatedCargoNix}/cargo/config" "$CARGO_HOME/config.toml"
          export CARGO_TARGET_DIR="$TMPDIR/target"
          export CARGO_TERM_COLOR=always

          ${exportEnv}

          runHook preBuild
          cd "${generatedCargoNix}/crate"
          ${command}
          runHook postBuild
        '';
        installPhase = ''
          mkdir -p "$out"
        '';
      };

    tui = cargoNix.workspaceMembers."kraai-tui".build;
  in {
    packages = {
      inherit tui;
      default = tui;
    };

    checks = {
      clippy = mkCargoCheck {
        name = "clippy";
        command = ''
          cargo clippy --workspace --all-targets -- --deny warnings
        '';
      };

      doc = mkCargoCheck {
        name = "doc";
        env.RUSTDOCFLAGS = "--deny warnings";
        command = ''
          cargo doc --workspace --no-deps
        '';
      };

      audit = mkCargoCheck {
        name = "audit";
        nativeBuildInputs = [pkgs.cargo-audit];
        command = ''
          cargo audit --db ${inputs.advisory-db} --no-fetch
        '';
      };

      deny = mkCargoCheck {
        name = "deny";
        nativeBuildInputs = [pkgs.cargo-deny];
        command = ''
          cargo deny check bans licenses sources
        '';
      };

      nextest = mkCargoCheck {
        name = "nextest";
        nativeBuildInputs = [pkgs.cargo-nextest];
        command = ''
          cargo nextest run --workspace --no-tests=pass
        '';
      };

      hakari = mkCargoCheck {
        name = "hakari";
        nativeBuildInputs = [pkgs.cargo-hakari];
        command = ''
          cargo hakari generate --diff  # kraai-workspace-hack Cargo.toml is up-to-date
          cargo hakari manage-deps --dry-run  # all workspace crates depend on kraai-workspace-hack
          cargo hakari verify
        '';
      };
    };
  };
}
