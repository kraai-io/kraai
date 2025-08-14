{inputs, ...}: {
  imports = [inputs.treefmt-nix.flakeModule];
  perSystem = {...}: {
    treefmt = {
      projectRootFile = "flake.nix";

      programs.alejandra.enable = true;
      programs.jsonfmt.enable = true;
      programs.just.enable = true;
      programs.prettier.enable = true;
      programs.rustfmt.enable = true;
      programs.taplo.enable = true;
      programs.toml-sort.enable = true;
      programs.yamlfmt.enable = true;
    };
  };
}
