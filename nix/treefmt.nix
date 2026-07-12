{inputs, ...}: {
  imports = [inputs.treefmt-nix.flakeModule];
  perSystem = {...}: {
    treefmt = {
      settings.excludes = [
        "Cargo.nix"
      ];

      projectRootFile = "flake.nix";

      programs.alejandra.enable = true;
      programs.deadnix.enable = true;
      programs.jsonfmt.enable = true;
      programs.just.enable = true;
      programs.rustfmt.enable = true;
      programs.taplo = {
        enable = true;
        settings = {
          formatting.reorder_arrays = false;
          formatting.reorder_keys = true;
        };
      };
      programs.yamlfmt.enable = true;
    };
  };
}
