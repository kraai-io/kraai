{inputs, ...}: {
  imports = [inputs.treefmt-nix.flakeModule];
  perSystem = {...}: {
    treefmt = {
      projectRootFile = "flake.nix";

      # Biome doesn't support Tailwind v4 syntax (@custom-variant, @theme inline, @apply)
      # See: https://github.com/biomejs/biome/issues/7899
      settings.excludes = [
        "apps/agent-desktop/src/styles/globals.css"
        "crates/kraai-workspace-hack/Cargo.toml"
        "Cargo.nix"
      ];

      programs.alejandra.enable = true;
      programs.deadnix.enable = true;
      programs.jsonfmt.enable = true;
      programs.just.enable = true;
      programs.biome.enable = true;
      programs.rustfmt.enable = true;
      programs.taplo = {
        enable = true;
        settings = {
          formatting.reorder_arrays = true;
          formatting.reorder_keys = true;
        };
      };
      programs.yamlfmt.enable = true;
    };
  };
}
