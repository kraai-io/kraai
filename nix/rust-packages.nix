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
    # crate2nix ignores Cargo profiles, so translate the release profile into rustc flags.
    releaseRustcOpts =
      lib.mapAttrs (
        key: value:
          {
            codegen-units = ["-C" "codegen-units=${toString value}"];
            lto = lib.optionals (value != false) [
              "-C"
              "lto=${
                if value == true
                then "fat"
                else value
              }"
            ];
            opt-level = ["-C" "opt-level=${toString value}"];
            strip = lib.optionals (value != false) [
              "-C"
              "strip=${
                if value == true
                then "symbols"
                else value
              }"
            ];
          }.${
            key
          } or (throw "nix/rust-packages.nix does not translate profile.release.${key}")
      )
      cargoToml.profile.release;
    gnuRustcOpts = cargoConfig.target."cfg(all(target_os = \"linux\", target_env = \"gnu\"))".rustflags;
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

    buildRustCrateForPkgs = pkgs: let
      buildRustCrate = pkgs.buildRustCrate.override {
        stdenv = withMold pkgs.stdenv;
        cargo = rustToolchain;
        rustc = rustToolchain;
        defaultCrateOverrides =
          pkgs.defaultCrateOverrides
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
      crate:
        buildRustCrate (crate
          // {
            extraRustcOpts =
              (crate.extraRustcOpts or [])
              ++ lib.optionals pkgs.stdenv.hostPlatform.isGnu gnuRustcOpts
              ++ lib.optionals (crate.release or true) cargoConfig.profile.release.rustflags
              ++ lib.optionals (crate.release or true) (
                lib.concatLists (lib.attrValues (
                  # rustc rejects LTO when a library also builds a non-rlib output. The binary's LTO
                  # still covers these crates through the bitcode in their rlibs.
                  if !(crate.procMacro or false) && lib.all (type: type == "lib" || type == "rlib") (crate.type or ["lib"])
                  then releaseRustcOpts
                  else removeAttrs releaseRustcOpts ["lto"]
                ))
              );
          });
    mkCargoNix = release:
      pkgs.callPackage ../Cargo.nix {
        inherit release buildRustCrateForPkgs;
      };

    cargoNix = mkCargoNix true;
    cargoCheckNix = mkCargoNix false;
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
      (withMold pkgs.stdenv).mkDerivation {
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
          outputHashes = {
            "ts-rs-macros-12.0.1" = "0zhwi4457nv5wyb9rzlway280sh6cwb6244ycfs630dk9hh8sdcy";
          };
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

    frontendPackage = member: binary:
      cargoNix.workspaceMembers.${member}.build.overrideAttrs (old: {
        nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.makeWrapper];
        postInstall =
          (old.postInstall or "")
          + ''
            wrapProgram "$out/bin/${binary}" \
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
            mainProgram = binary;
          };
      });

    kraai = frontendPackage "kraai-tui" "kraai";
    kraai-acp = frontendPackage "kraai-acp" "kraai-acp";
    kraai-headless = frontendPackage "kraai-headless" "kraai-headless";
    kraai-perf = frontendPackage "kraai-perf" "kraai-perf";

    kraai-eval = cargoNix.workspaceMembers."kraai-eval".build.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.makeWrapper];
      postInstall =
        (old.postInstall or "")
        + ''
          wrapProgram "$out/bin/kraai-eval" \
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
