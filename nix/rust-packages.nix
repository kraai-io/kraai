{inputs, ...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    fs = lib.fileset;
    workspaceManifest = lib.importTOML ../Cargo.toml;
    workspaceMembers =
      map (
        memberPath: let
          manifest = lib.importTOML (../. + "/${memberPath}/Cargo.toml");
        in {
          inherit memberPath;
          name = manifest.package.name;
        }
      )
      workspaceManifest.workspace.members;
    rustSources = fs.toSource {
      root = ../.;
      fileset = fs.unions [
        ../Cargo.lock
        ../Cargo.toml
        ../crates
      ];
    };

    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.latest.default);

    commonArgs = {
      src = craneLib.cleanCargoSource rustSources;
      strictDeps = true;
      nativeBuildInputs = with pkgs; [
        pkg-config
      ];
      buildInputs = with pkgs; [
        openssl
      ];
    };

    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

    tuiPackage = craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
        cargoExtraArgs = "-p tui";
        doCheck = true;
      });

    cargoTestChecks = builtins.listToAttrs (map (member: {
        name = "cargo-test-${member.name}";
        value = craneLib.cargoTest (commonArgs
          // {
            inherit cargoArtifacts;
            cargoExtraArgs = "-p ${member.name}";
          });
      })
      workspaceMembers);
    cargoClippyChecks = builtins.listToAttrs (map (member: {
        name = "cargo-clippy-${member.name}";
        value = craneLib.cargoClippy (commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "-p ${member.name} --all-targets -- --deny warnings";
          });
      })
      workspaceMembers);
  in {
    packages = rec {
      tui = tuiPackage;
      default = tui;
    };

    checks =
      cargoTestChecks
      // cargoClippyChecks;
  };
}
