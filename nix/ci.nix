{
  self,
  config,
  lib,
  inputs,
  ...
}: {
  flake.hydraJobs = let
    allJobs =
      {
      }
      // (
        lib.genAttrs config.systems (system: {
          checks = self.checks.${system};
          packages = self.packages.${system};
          devShells = self.devShells.${system};
        })
      );
  in
    allJobs
    // {
      allJobs = inputs.nixpkgs.legacyPackages.${builtins.head config.systems}.releaseTools.aggregate {
        name = "allJobs";
        constituents = lib.collect lib.isDerivation allJobs;
      };
    };
}
