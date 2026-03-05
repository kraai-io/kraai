{inputs, ...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.latest.default);
    src = craneLib.cleanCargoSource ../.;

    commonArgs = {
      inherit src;
      pname = "agent";
      version = "0.0.0";
      strictDeps = true;
      nativeBuildInputs = with pkgs; [
        pkg-config
      ];
      buildInputs = with pkgs; [
        openssl
      ];
    };

    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

    individualCrateArgs = crate:
      commonArgs
      // {
        inherit cargoArtifacts;
        inherit
          (craneLib.crateNameFromCargoToml {
            src = craneLib.cleanCargoSource crate;
          })
          version
          pname
          ;
        doCheck = false;
      };

    fileSetForCrate = crate:
      lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions [
          ../Cargo.toml
          ../Cargo.lock
          (craneLib.fileset.commonCargoSources ../crates)
          (craneLib.fileset.commonCargoSources crate)
        ];
      };

    tui = craneLib.buildPackage (individualCrateArgs ../crates/tui
      // {
        cargoExtraArgs = "-p tui";
        src = fileSetForCrate ../crates/tui;
      });
  in {
    packages = {
      inherit tui;
      default = tui;
    };

    checks = {
      inherit tui;

      # clippy = craneLib.cargoClippy (
      #   commonArgs
      #   // {
      #     inherit cargoArtifacts;
      #     cargoClippyExtraArgs = "--all-targets -- --deny warnings";
      #   }
      # );

      # doc = craneLib.cargoDoc (
      #   commonArgs
      #   // {
      #     inherit cargoArtifacts;
      #     env.RUSTDOCFLAGS = "--deny warnings";
      #   }
      # );

      # audit = craneLib.cargoAudit {
      #   inherit src;
      #   inherit (inputs) advisory-db;
      # };

      # deny = craneLib.cargoDeny {
      #   inherit src;
      # };

      # nextest = craneLib.cargoNextest (
      #   commonArgs
      #   // {
      #     inherit cargoArtifacts;
      #     partitions = 1;
      #     partitionType = "count";
      #     cargoNextestPartitionsExtraArgs = "--no-tests=pass";
      #   }
      # );

      # hakari = craneLib.mkCargoDerivation {
      #   inherit src;
      #   pname = "hakari";
      #   cargoArtifacts = null;
      #   doInstallCargoArtifacts = false;

      #   buildPhaseCargoCommand = ''
      #     cargo hakari generate --diff  # workspace-hack Cargo.toml is up-to-date
      #     cargo hakari manage-deps --dry-run  # all workspace crates depend on workspace-hack
      #     cargo hakari verify
      #   '';

      #   nativeBuildInputs = [
      #     pkgs.cargo-hakari
      #   ];
      # };
    };
  };
}
