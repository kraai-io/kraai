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
      npmDeps = pkgs.importNpmLock {
        npmRoot = ../packages/eval-viewer;
      };
      npmConfigHook = pkgs.importNpmLock.npmConfigHook;
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
