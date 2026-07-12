{inputs, ...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
      extensions = ["llvm-tools-preview"];
    };
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.lock
        ../Cargo.toml
        ../Cargo.nix
        ../crates
        ../deny.toml
        ../justfile
      ];
    };
    workspaceMembers =
      map
      (memberPath: let
        cargoToml = lib.importTOML (../. + "/${memberPath}/Cargo.toml");
      in {
        name = cargoToml.package.name;
        procMacro = cargoToml.lib.proc-macro or false;
      })
      (lib.importTOML ../Cargo.toml).workspace.members;
    crate2nixTestMemberNames =
      map
      (member: member.name)
      (lib.filter (member: !member.procMacro) workspaceMembers);
    # crate2nix can build proc-macro crates, but its integration-test wrapper
    # passes a nonexistent root-crate rlib to rustc when the tested workspace
    # member itself is a proc-macro crate.
    cargoTestMemberNames =
      map
      (member: member.name)
      (lib.filter (member: member.procMacro) workspaceMembers);

    mkCargoNix = release:
      pkgs.callPackage ../Cargo.nix {
        inherit release;
        buildRustCrateForPkgs = pkgs:
          pkgs.buildRustCrate.override {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
      };

    cargoNix = mkCargoNix true;
    cargoCheckNix = mkCargoNix false;

    mkCargoCheck = {
      name,
      command,
      nativeBuildInputs ? [],
      buildInputs ? [],
      env ? {},
    }:
      pkgs.stdenv.mkDerivation {
        pname = name;
        version = "0.0.0";
        inherit src;
        strictDeps = true;
        nativeBuildInputs =
          [
            rustToolchain
            pkgs.rustPlatform.cargoSetupHook
            pkgs.pkg-config
          ]
          ++ nativeBuildInputs;
        buildInputs =
          [
            pkgs.openssl
          ]
          ++ buildInputs;
        cargoDeps = pkgs.rustPlatform.importCargoLock {
          lockFile = ../Cargo.lock;
        };
        buildPhase = let
          exportEnv = lib.concatLines (
            lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg value}") env
          );
        in ''
          export HOME="$TMPDIR/home"
          mkdir -p "$HOME"
          export CARGO_TARGET_DIR="$TMPDIR/target"
          export CARGO_TERM_COLOR=always

          ${exportEnv}

          runHook preBuild
          ${command}
          runHook postBuild
        '';
        installPhase = ''
          mkdir -p "$out"
        '';
      };

    kraai = cargoNix.workspaceMembers."kraai-tui".build.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.makeWrapper];
      postInstall =
        (old.postInstall or "")
        + ''
          wrapProgram "$out/bin/kraai" --prefix PATH : ${lib.makeBinPath [pkgs.bubblewrap]}
        '';
      meta =
        (old.meta or {})
        // {
          mainProgram = "kraai";
        };
    });

    kraai-eval = cargoNix.workspaceMembers."kraai-eval".build.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.makeWrapper];
      postInstall =
        (old.postInstall or "")
        + ''
          wrapProgram "$out/bin/kraai-eval" --prefix PATH : ${lib.makeBinPath [
            pkgs.bubblewrap
            pkgs.coreutils
            pkgs.git
            pkgs.gnutar
            pkgs.systemd
          ]}
        '';
      meta =
        (old.meta or {})
        // {
          mainProgram = "kraai-eval";
        };
    });

    workspaceTestChecks = builtins.listToAttrs (
      map
      (name:
        lib.nameValuePair "test-${name}" (cargoCheckNix.workspaceMembers.${name}.build.override {
          runTests = true;
          testPreRun = ''
            export SSL_CERT_FILE=${lib.escapeShellArg "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"}
          '';
        }))
      crate2nixTestMemberNames
    );
    cargoTestChecks = builtins.listToAttrs (
      map
      (name:
        lib.nameValuePair "test-${name}" (mkCargoCheck {
          name = "test-${name}";
          nativeBuildInputs = [pkgs.cargo-nextest];
          command = ''
            cargo nextest run -p ${lib.escapeShellArg name} --no-tests=pass
          '';
        }))
      cargoTestMemberNames
    );
  in {
    packages = {
      inherit kraai kraai-eval;
      default = kraai;
    };

    checks =
      workspaceTestChecks
      // cargoTestChecks
      // {
        clippy = mkCargoCheck {
          name = "clippy";
          command = ''
            ${pkgs.just}/bin/just lint
          '';
        };

        doc = mkCargoCheck {
          name = "doc";
          env.RUSTDOCFLAGS = "--deny warnings";
          command = ''
            cargo doc --workspace --no-deps
          '';
        };

        audit = mkCargoCheck {
          name = "audit";
          nativeBuildInputs = [pkgs.cargo-audit];
          command = ''
            cargo audit --db ${inputs.advisory-db} --no-fetch
          '';
        };

        deny = mkCargoCheck {
          name = "deny";
          nativeBuildInputs = [pkgs.cargo-deny];
          command = ''
            cargo deny check bans licenses sources
          '';
        };
      };
  };
}
