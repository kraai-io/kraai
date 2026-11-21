{...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    # Builds the Vite app in packages/<name>: installs its locked npm
    # dependencies, runs its typecheck and tests, and installs dist/.
    # `assets` lists repository paths outside the package that the build reads.
    viteApp = {
      name,
      pname,
      assets ? [],
    }: let
      dir = ../packages + "/${name}";
      package = lib.fileset.difference dir (lib.fileset.unions [
        (lib.fileset.maybeMissing (dir + "/node_modules"))
        (lib.fileset.maybeMissing (dir + "/dist"))
      ]);
      src = lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions ([package] ++ assets);
      };
    in
      pkgs.buildNpmPackage {
        inherit pname src;
        version = (lib.importJSON (dir + "/package.json")).version;
        sourceRoot = "${src.name}/packages/${name}";
        npmDeps = pkgs.importNpmLock {
          npmRoot = dir;
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

    viewer = viteApp {
      name = "eval-viewer";
      pname = "kraai-eval-viewer";
    };
    site = viteApp {
      name = "site";
      pname = "kraai-site";
      assets = [../assets/brand/svg ../assets/readme/logos];
    };
  in {
    packages.kraai-eval-viewer = viewer;
    packages.kraai-site = site;
    checks.eval-viewer = viewer;
  };
}
