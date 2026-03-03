{inputs, ...}: {
  perSystem = {
    lib,
    system,
    ...
  }: let
    fs = lib.fileset;
    workspaceManifest = lib.importTOML ../Cargo.toml;
    tuiManifest = lib.importTOML ../crates/tui/Cargo.toml;
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

    tuiPackage = inputs.drowse.lib.${system}.crate2nix {
      pname = tuiManifest.package.name;
      inherit (tuiManifest.package) version;
      src = rustSources;
      select = "project: project.workspaceMembers.tui.build.override {runTests = true;}";
    };
    mkWorkspaceTest = member:
      inputs.drowse.lib.${system}.crate2nix {
        pname = "${member.name}-tests";
        inherit (member) version;
        src = rustSources;
        select = ''
          project:
          project.workspaceMembers."${member.name}".build.override {
            runTests = true;
          }
        '';
      };
    workspaceTestChecks = builtins.listToAttrs (map (member: {
        name = "test-${member.name}";
        value = mkWorkspaceTest member;
      })
      workspaceMembers);
  in {
    packages = rec {
      tui = tuiPackage;
      default = tui;
    };

    checks = workspaceTestChecks;
  };
}
