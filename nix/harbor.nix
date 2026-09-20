{inputs, ...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    workspace = inputs.uv2nix.lib.workspace.loadWorkspace {
      workspaceRoot = ../evals/harbor;
    };
    pythonSet =
      (pkgs.callPackage inputs.pyproject-nix.build.packages {
        python = pkgs.python312;
      }).overrideScope (lib.composeManyExtensions [
        inputs.pyproject-build-systems.overlays.wheel
        (workspace.mkPyprojectOverlay {sourcePreference = "wheel";})
        (final: prev: {
          kraai-harbor = prev.kraai-harbor.overrideAttrs (old: {
            nativeBuildInputs = old.nativeBuildInputs ++ final.resolveBuildSystem {hatchling = [];};
          });
        })
      ]);
    environment = pythonSet.mkVirtualEnv "kraai-harbor-tests" workspace.deps.default;
  in {
    checks.harbor =
      pkgs.runCommand "kraai-harbor-tests" {
        nativeBuildInputs = [environment];
      } ''
        export HOME="$TMPDIR"
        export PYTHONDONTWRITEBYTECODE=1
        python -m unittest discover -s ${../evals/harbor/tests}
        touch "$out"
      '';
  };
}
