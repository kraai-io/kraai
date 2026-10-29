{inputs, ...}: {
  perSystem = {
    pkgs,
    system,
    ...
  }: {
    _module.args = {
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [(import inputs.rust-overlay)];
      };
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
      evalRustToolchain = pkgs.rust-bin.stable.latest.default;
      withMold = stdenv:
        if stdenv.hostPlatform.isLinux
        then pkgs.stdenvAdapters.useMoldLinker stdenv
        else stdenv;
    };
  };
}
