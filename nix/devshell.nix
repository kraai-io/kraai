{inputs, ...}: {
  perSystem = {
    system,
    pkgs,
    ...
  }: {
    _module.args.pkgs = import inputs.nixpkgs {
      inherit system;
      overlays = [(import inputs.rust-overlay)];
    };
    devShells = {
      default = pkgs.mkShell {
        stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.clangStdenv;
        buildInputs = with pkgs; [
          cargo-edit
          cargo-msrv
          rust-bin.stable.latest.default
          rust-analyzer

          pkg-config
          openssl
        ];
      };
    };
  };
}
