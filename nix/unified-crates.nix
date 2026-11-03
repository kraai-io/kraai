# crate2nix resolves features separately for every root crate, and its test builds resolve a second
# graph with the root's dev-dependencies. Every package and test check then built its own copy of
# most dependencies. These graphs resolve features once for the whole workspace, like cargo's
# `resolver.feature-unification = "workspace"`.
{
  lib,
  stdenv,
}: {
  cargoNix,
  buildRustCrateForPkgs,
  # Include every member's dev-dependencies in the resolution, so test builds share the graph.
  withDevDependencies,
}: let
  inherit (cargoNix.internal) crates;
  memberIds = lib.mapAttrsToList (_: member: member.packageId) cargoNix.workspaceMembers;
  rootId = "kraai-unified-workspace-root";
  root = {
    crateName = rootId;
    version = "0.0.0";
    edition = "2024";
    dependencies =
      map (packageId: {
        name = crates.${packageId}.crateName;
        inherit packageId;
      })
      memberIds
      ++ lib.optionals withDevDependencies (
        lib.concatMap (packageId: crates.${packageId}.devDependencies or []) memberIds
      );
  };
  graph =
    (cargoNix.internal.builtRustCratesWithFeatures {
      packageId = rootId;
      features = [];
      crateConfigs = crates // {${rootId} = root;};
      buildRustCrateForPkgsFunc = buildRustCrateForPkgs;
      runTests = false;
    }).crates;
  testTarget = cargoNix.internal.makeDefaultTarget stdenv.hostPlatform // {test = true;};

  testCrate = packageId:
    graph.${packageId}.override (old: let
      devDependencies = cargoNix.internal.filterEnabledDependencies {
        dependencies = crates.${packageId}.devDependencies or [];
        inherit (old) features;
        target = testTarget;
      };
      devRenames = lib.mapAttrs (_:
        map (dep: {
          inherit (dep) rename;
          inherit (crates.${dep.packageId}) version;
        })) (lib.groupBy (dep: dep.name) (lib.filter (dep: dep ? rename) devDependencies));
    in {
      dependencies = old.dependencies ++ map (dep: graph.${dep.packageId}) devDependencies;
      crateRenames = lib.zipAttrsWith (_: lib.concatLists) [(old.crateRenames or {}) devRenames];
    });
in {
  crates = lib.genAttrs memberIds (packageId: graph.${packageId});
  testCrates = lib.genAttrs memberIds testCrate;
  withTests = packageId: {
    testCrateFlags ? [],
    testInputs ? [],
    testPreRun ? "",
    testPostRun ? "",
  }:
    cargoNix.internal.crateWithTest {
      crate = graph.${packageId};
      testCrate = testCrate packageId;
      inherit testCrateFlags testInputs testPreRun testPostRun;
    };
}
