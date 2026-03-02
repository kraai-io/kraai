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
          rust-bin.stable.latest.default
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
