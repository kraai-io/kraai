{inputs, ...}: {
  perSystem = {
    lib,
    pkgs,
    ...
  }: let
    rustToolchain = pkgs.rust-bin.stable.latest.default;
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../.cargo/audit.toml
        ../Cargo.lock
        ../Cargo.toml
        ../Cargo.nix
        ../crates
        ../evals
        ../deny.toml
        ../justfile
        ../packages/runtime/index.cjs
        ../packages/runtime/package.json
        ../packages/runtime/scripts
        ../packages/runtime/test
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
    workspaceTestInputs = {
      "kraai-eval" = [pkgs.git pkgs.clang pkgs.coreutils pkgs.findutils rustToolchain];
    };
    darwinNestedSandboxTests = {
      "kraai-sandbox" = ["tests::macos::"];
      "kraai-nushell-runtime" = [
        "private_transport_crosses_the_sandbox_boundary"
        "native_commands_remain_registered_when_the_sandbox_denies_the_operation"
      ];
    };

    buildRustCrateForPkgs = pkgs:
      pkgs.buildRustCrate.override {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };
    mkCargoNix = release:
      pkgs.callPackage ../Cargo.nix {
        inherit release buildRustCrateForPkgs;
      };

    cargoNix = mkCargoNix true;
    cargoCheckNix = mkCargoNix false;
    nushellHost = cargoNix.workspaceMembers."kraai-nushell-runtime".build;
    vmTestBinaries = name:
      ((cargoCheckNix.internal.builtRustCratesWithFeatures {
          packageId = name;
          features = ["default"];
          buildRustCrateForPkgsFunc = buildRustCrateForPkgs;
          runTests = true;
        }).crates.${
          name
        }).override {buildTests = true;};

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
          install -Dm755 ${nushellHost}/bin/kraai-nushell-host "$out/bin/kraai-nushell-host"
          wrapProgram "$out/bin/kraai" \
            --set-default SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
            --prefix PATH : ${lib.makeBinPath (
            [pkgs.ripgrep]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [pkgs.bubblewrap]
          )} \
            --set KRAAI_SCRIPT_RUNTIME_ROOTS /nix/store
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
          wrapProgram "$out/bin/kraai-eval" \
            --set-default SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
            --set-default KRAAI_EVAL_TASKS ${../evals/tasks} \
            --set-default KRAAI_EVAL_HARBOR ${../evals/harbor} \
            --prefix PATH : ${lib.makeBinPath [
            kraai
            pkgs.bubblewrap
            pkgs.clang
            pkgs.coreutils
            pkgs.findutils
            pkgs.git
            pkgs.gnutar
            pkgs.docker-client
            pkgs.docker-compose
            pkgs.gnused
            pkgs.pkg-config
            pkgs.ripgrep
            pkgs.systemd
            pkgs.nix
            pkgs.uv
            pkgs.python312
            rustToolchain
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
      (name: let
        tested = cargoCheckNix.workspaceMembers.${name}.build.override {
          runTests = true;
          testInputs = workspaceTestInputs.${name} or [];
          testCrateFlags =
            lib.optionals pkgs.stdenv.hostPlatform.isDarwin (
              lib.concatMap (test: ["--skip" test]) (darwinNestedSandboxTests.${name} or [])
            )
            ++ lib.optionals (name == "kraai-nushell-runtime") [
              "--skip"
              "sandboxed_host_accepts_a_workspace_symlink_alias"
            ]
            ++ lib.optionals (name == "kraai-sandbox") [
              "--skip"
              "capability_matrix"
              "--skip"
              "linked_metadata_obeys_the_same_capabilities"
            ];
          testPreRun = ''
            export SSL_CERT_FILE=${lib.escapeShellArg "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"}
            ${lib.optionalString (name == "kraai-eval") ''
              export KRAAI_EVAL_ASSETS=${../evals}
            ''}
          '';
        };
      in
        lib.nameValuePair "test-${name}" (
          if pkgs.stdenv.hostPlatform.isDarwin
          then tested.test.overrideAttrs {__darwinAllowLocalNetworking = true;}
          else tested
        ))
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
    packages =
      {
        inherit kraai;
        default = kraai;
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {inherit kraai-eval;};

    checks =
      workspaceTestChecks
      // cargoTestChecks
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        sandbox-vm = import ./sandbox-vm.nix {
          inherit pkgs;
          sandboxTests = vmTestBinaries "kraai-sandbox";
          runtimeTests = vmTestBinaries "kraai-nushell-runtime";
        };
      }
      // {
        node = mkCargoCheck {
          name = "node";
          nativeBuildInputs =
            [pkgs.nodejs pkgs.typescript]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [pkgs.binutils pkgs.bubblewrap]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [pkgs.darwin.cctools];
          env.SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          command = ''
            ${pkgs.just}/bin/just check-node ${lib.optionalString pkgs.stdenv.hostPlatform.isDarwin "test:portable"}
          '';
        };
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
