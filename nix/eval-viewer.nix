{...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    viewer = pkgs.buildNpmPackage {
      pname = "kraai-eval-viewer";
      version = (lib.importJSON ../packages/eval-viewer/package.json).version;
      src = lib.cleanSourceWith {
        src = ../packages/eval-viewer;
        filter = path: type:
          !(type == "directory" && builtins.elem (baseNameOf path) ["node_modules" "dist"]);
      };
      npmDepsHash = "sha256-dAt87PXpj0WCOsO3+Bwl4IvaddsyitwE7JOdM9Tx5Dg=";
      doCheck = true;
      checkPhase = ''
        runHook preCheck
        npm run typecheck
        npm test
        runHook postCheck
      '';
      installPhase = ''
        runHook preInstall
        mkdir -p "$out"
        cp -r dist/. "$out/"
        runHook postInstall
      '';
    };
  in {
    packages.kraai-eval-viewer = viewer;
    checks.eval-viewer = viewer;
  };
}
