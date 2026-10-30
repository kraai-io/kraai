{inputs, ...}: {
  perSystem = {
    lib,
    system,
    pkgs,
    rustToolchain,
    withMold,
    ...
  }: {
    devShells = {
      default =
        (pkgs.mkShell.override {
          stdenv = withMold pkgs.clangStdenv;
        }) {
          buildInputs = with pkgs;
            [
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
              inputs.crate2nix.packages.${system}.default
              samply
              rustToolchain
              rust-analyzer

              just
              uv
              python312
              nodejs
              typescript

              ripgrep
              findutils
              pkg-config
              openssl
            ]
            ++ lib.optionals stdenv.hostPlatform.isLinux [bubblewrap binutils docker-client docker-compose perf inferno valgrind]
            ++ lib.optionals stdenv.hostPlatform.isDarwin [darwin.cctools];
          # Nightly cargo defaults to a new build layout and metadata-only rlibs. The eval graders'
          # hidden-test builds, which kraai-eval's tests run with this cargo, need the stable ones.
          env = {
            CARGO_UNSTABLE_BUILD_DIR_NEW_LAYOUT = "false";
            CARGO_UNSTABLE_EMBED_METADATA = "true";
          };
        };
    };
  };
}
