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
          cargo-watch
          cargo-audit
          cargo-deny
          cargo-nextest
          cargo-llvm-cov
          cargo-geiger
          cargo-crev
          cargo-flamegraph
          cargo-autoinherit
          samply
          (rust-bin.stable.latest.default.override {
            extensions = ["llvm-tools-preview"];
          })
          rust-analyzer

          just

          ripgrep
          pkg-config
          openssl
        ];
      };
    };
  };
}
