{inputs, ...}: {
  perSystem = {
    evalRustToolchain,
    lib,
    pkgs,
    rustToolchain,
    self',
    withMold,
    ...
  }: let
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../.cargo
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
    cargoToml = lib.importTOML ../Cargo.toml;
    cargoConfig = lib.importTOML ../.cargo/config.toml;
    workspaceMembers =
      map
      (memberPath: let
        memberCargoToml = lib.importTOML (../. + "/${memberPath}/Cargo.toml");
      in {
        name = memberCargoToml.package.name;
        procMacro = memberCargoToml.lib.proc-macro or false;
      })
      cargoToml.workspace.members;
    cargoProfiles = import ./cargo-profiles.nix {
      inherit lib;
      manifest = cargoToml;
      config = cargoConfig;
      memberNames = map (member: member.name) workspaceMembers;
    };
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
      "kraai-eval" = [pkgs.git pkgs.clang pkgs.coreutils pkgs.findutils evalRustToolchain];
    };
    darwinNestedSandboxTests = {
      "kraai-sandbox" = ["tests::macos::"];
      "kraai-nushell-runtime" = [
        "private_transport_crosses_the_sandbox_boundary"
        "native_commands_remain_registered_when_the_sandbox_denies_the_operation"
      ];
    };

    mkBuildRustCrate = cratePkgs: let
      buildRustCrate = cratePkgs.buildRustCrate.override {
        stdenv = withMold cratePkgs.stdenv;
        cargo = rustToolchain;
        rustc = rustToolchain;
        defaultCrateOverrides =
          cratePkgs.defaultCrateOverrides
          // {
            rmcp = _: {CARGO_CRATE_NAME = "rmcp";};
            # Its macro emits items in HashMap order, so every build of a parser crate gets a
            # different crate hash and cached dependents stop linking against a rebuilt copy.
            pest_consume_macros = _: {
              postPatch = ''
                substituteInPlace src/make_parser.rs --replace-fail \
                  'use std::collections::HashMap;' \
                  'use std::collections::BTreeMap as HashMap;'
              '';
            };
          };
      };
    in
      crate: let
        procMacro = crate.procMacro or false;
        rustcOpts = args:
          cargoProfiles.rustcOpts ({
              profile =
                if crate.release or true
                then "release"
                else "test";
              inherit (crate) crateName;
            }
            // args);
        buildUnitOpts = rustcOpts {
          buildUnit = true;
          withLto = false;
        };
      in
        buildRustCrate (crate
          // {
            extraRustcOpts =
              (crate.extraRustcOpts or [])
              ++ rustcOpts {
                withLto = !procMacro && lib.all (type: type == "lib" || type == "rlib") (crate.type or ["lib"]);
              };
            extraRustcOptsForBuildRs = (crate.extraRustcOptsForBuildRs or []) ++ buildUnitOpts;
          }
          // lib.optionalAttrs procMacro {
            extraRustcOptsForProcMacro = (crate.extraRustcOptsForProcMacro or crate.extraRustcOpts or []) ++ buildUnitOpts;
          });
    # Cargo.nix calls this once per crate. Rebuilding the mold stdenv and the buildRustCrate
    # override on every call dominated evaluation time.
    hostBuildRustCrate = mkBuildRustCrate pkgs;
    buildRustCrateForPkgs = cratePkgs:
      if cratePkgs.stdenv.hostPlatform.config == pkgs.stdenv.hostPlatform.config
      then hostBuildRustCrate
      else mkBuildRustCrate cratePkgs;
    mkCargoNix = release:
      pkgs.callPackage ../Cargo.nix {
        inherit release buildRustCrateForPkgs;
      };

    unifiedCrates = import ./unified-crates.nix {
      inherit lib;
      inherit (pkgs) stdenv;
    };
    releaseCrates = unifiedCrates {
      cargoNix = mkCargoNix true;
      inherit buildRustCrateForPkgs;
      withDevDependencies = false;
    };
    debugCrates = unifiedCrates {
      cargoNix = mkCargoNix false;
      inherit buildRustCrateForPkgs;
      withDevDependencies = true;
    };
    vmTestBinaries = name: debugCrates.testCrates.${name}.override {buildTests = true;};

    cargoArtifacts = import ./cargo-artifacts.nix {
      inherit lib;
      inherit (pkgs) runCommand;
      root = ../.;
      manifest = cargoToml;
    };

    # dependenciesCommand compiles the check's dependencies against stubbed members ahead of time.
    # It must resolve the same features and profile as command, or cargo rebuilds them anyway.
    mkCargoCheck = {
      name,
      command,
      dependenciesCommand ? null,
      nativeBuildInputs ? [],
      buildInputs ? [],
      env ? {},
    }: let
      mkCargoDerivation = {script, ...} @ args:
        (withMold pkgs.stdenv).mkDerivation ((removeAttrs args ["script"])
          // {
            version = "0.0.0";
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
              outputHashes = {
                "ts-rs-macros-12.0.1" = "0zhwi4457nv5wyb9rzlway280sh6cwb6244ycfs630dk9hh8sdcy";
              };
            };
            buildPhase = ''
              export HOME="$TMPDIR/home"
              mkdir -p "$HOME"
              export CARGO_TARGET_DIR="$TMPDIR/target"
              export CARGO_TERM_COLOR=always
              export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
              export NEXTEST_TEST_THREADS="$NIX_BUILD_CORES"

              ${lib.concatLines (lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg value}") env)}

              runHook preBuild
              ${script}
              runHook postBuild
            '';
          });
      dependencies = mkCargoDerivation {
        pname = "${name}-dependencies";
        src = cargoArtifacts.source;
        script = ''
          ${dependenciesCommand}
          ${cargoArtifacts.removeMembers ''"$CARGO_TARGET_DIR"''}
        '';
        installPhase = ''
          cp -r "$CARGO_TARGET_DIR" "$out"
        '';
        # Stripping or patching would change the artifacts cargo already fingerprinted.
        dontFixup = true;
      };
    in
      mkCargoDerivation {
        pname = name;
        inherit src;
        script =
          lib.optionalString (dependenciesCommand != null) (cargoArtifacts.restore dependencies ''"$CARGO_TARGET_DIR"'')
          + command;
        installPhase = ''
          mkdir -p "$out"
        '';
        passthru = lib.optionalAttrs (dependenciesCommand != null) {inherit dependencies;};
      };

    # Wraps a copy of the release crate's binaries instead of overriding the crate itself. An
    # override is a second derivation that recompiles the crate, and other members still link the
    # unwrapped one. Copying keeps current_exe() and its sibling binaries inside the package.
    wrapReleaseBinary = {
      member,
      binary,
      wrapperArgs,
    }: let
      crate = releaseCrates.crates.${member};
    in
      pkgs.runCommand "${binary}-${crate.version}" {
        nativeBuildInputs = [pkgs.makeWrapper];
        passthru.unwrapped = crate;
        meta = (crate.meta or {}) // {mainProgram = binary;};
      } ''
        mkdir -p "$out/bin"
        cp ${crate}/bin/* "$out/bin/"
        wrapProgram "$out/bin/${binary}" ${wrapperArgs}
      '';

    frontendPackage = member: binary:
      wrapReleaseBinary {
        inherit member binary;
        wrapperArgs = ''
          --set-default SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
          --prefix PATH : ${lib.makeBinPath (
            [pkgs.ripgrep]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [pkgs.bubblewrap]
          )} \
          --set KRAAI_SCRIPT_RUNTIME_ROOTS /nix/store
        '';
      };

    kraai = frontendPackage "kraai-tui" "kraai";
    kraai-acp = frontendPackage "kraai-acp" "kraai-acp";
    kraai-headless = frontendPackage "kraai-headless" "kraai-headless";
    kraai-perf = frontendPackage "kraai-perf" "kraai-perf";

    kraai-eval = wrapReleaseBinary {
      member = "kraai-eval";
      binary = "kraai-eval";
      wrapperArgs = ''
        --set-default SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
        --set-default KRAAI_EVAL_TASKS ${../evals/tasks} \
        --set-default KRAAI_EVAL_HARBOR ${../evals/harbor} \
        --set-default KRAAI_EVAL_VIEWER_ASSETS ${self'.packages.kraai-eval-viewer} \
        --prefix PATH : ${lib.makeBinPath [
          kraai-headless
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
          evalRustToolchain
        ]}
      '';
    };

    workspaceTestChecks = builtins.listToAttrs (
      map
      (name: let
        tested = debugCrates.withTests name {
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
        inherit kraai kraai-acp kraai-headless kraai-perf;
        default = kraai;
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {inherit kraai-eval;};

    checks =
      workspaceTestChecks
      // cargoTestChecks
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        sessions-vm = import ./sessions-vm.nix {
          inherit pkgs kraai-acp;
        };
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
          # packages/runtime/scripts/build.mjs builds two members. With workspace feature
          # unification, building all of them compiles the same dependencies.
          dependenciesCommand = ''
            cargo build --locked --workspace
          '';
          command = ''
            ${pkgs.just}/bin/just check-node ${lib.optionalString pkgs.stdenv.hostPlatform.isDarwin "test:portable"}
          '';
        };
        clippy = mkCargoCheck {
          name = "clippy";
          dependenciesCommand = ''
            cargo clippy --workspace --all-targets --all-features
          '';
          command = ''
            ${pkgs.just}/bin/just lint
          '';
        };

        doc = mkCargoCheck {
          name = "doc";
          env.RUSTDOCFLAGS = "--deny warnings";
          dependenciesCommand = ''
            cargo doc --workspace --no-deps
          '';
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
