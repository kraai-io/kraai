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
          version = manifest.package.version;
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

    workspaceVersion = (builtins.head workspaceMembers).version;

    cargoArtifacts = craneLib.buildDepsOnly (commonArgs
      // {
        pname = "workspace-deps";
        version = workspaceVersion;
      });

    tuiMember = lib.findFirst (member: member.name == "tui") null workspaceMembers;
    tuiVersion =
      if tuiMember == null
      then throw "workspace member 'tui' not found"
      else tuiMember.version;

    tuiPackage = craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
        pname = "tui";
        version = tuiVersion;
        cargoExtraArgs = "-p tui";
        doCheck = true;
      });

    cargoTestChecks = builtins.listToAttrs (map (member: {
        name = "${member.name}";
        value = craneLib.cargoTest (commonArgs
          // {
            inherit cargoArtifacts;
            pname = "${member.name}";
            inherit (member) version;
            cargoExtraArgs = "-p ${member.name}";
          });
      })
      workspaceMembers);
    cargoClippyChecks = builtins.listToAttrs (map (member: {
        name = "${member.name}";
        value = craneLib.cargoClippy (commonArgs
          // {
            inherit cargoArtifacts;
            pname = "${member.name}";
            inherit (member) version;
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
