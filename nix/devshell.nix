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
          cargo-hakari
          cargo-nextest
          cargo-llvm-cov
          cargo-geiger
          cargo-crev
          cargo-flamegraph
          samply
          (rust-bin.stable.latest.default.override {
            extensions = ["llvm-tools-preview"];
          })
          rust-analyzer

          pnpm
          nodejs

          just

          ripgrep
          wine
          pkg-config
          openssl
          at-spi2-atk
          atkmm
          cairo
          gdk-pixbuf
          glib
          gtk3
          harfbuzz
          librsvg
          libsoup_3
          pango
          webkitgtk_4_1
        ];
      };
    };
  };
}
