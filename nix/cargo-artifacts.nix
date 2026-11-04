# Cargo-based checks compile every dependency from scratch. This builds a check's dependencies
# once, against a copy of the workspace where every target is a stub, so the result only changes
# with the manifests and Cargo.lock. The check copies that target directory in, and cargo then
# rebuilds the workspace members alone.
{
  lib,
  runCommand,
  root,
  manifest,
}: let
  memberPaths = manifest.workspace.members;
  members =
    map (path: let
      memberManifest = lib.importTOML (root + "/${path}/Cargo.toml");
      exists = file: builtins.pathExists (root + "/${path}/${file}");
      named = kind: dir: map (target: target.path or "${dir}/${target.name}.rs") (memberManifest.${kind} or []);
      withHarness = kind: dir:
        map (target: {
          file = target.path or "${dir}/${target.name}.rs";
          main = !(target.harness or true);
        }) (memberManifest.${kind} or []);
      build = memberManifest.package.build or null;
      libPath =
        memberManifest.lib.path
        or (
          if memberManifest ? lib || exists "src/lib.rs"
          then "src/lib.rs"
          else null
        );
      mainFiles =
        lib.optional (exists "src/main.rs") "src/main.rs"
        ++ named "bin" "src/bin"
        ++ named "example" "examples"
        ++ lib.optional (lib.isString build) build
        ++ lib.optional (build == null && exists "build.rs") "build.rs";
    in {
      inherit path;
      name = memberManifest.package.name;
      libName = memberManifest.lib.name or (lib.replaceStrings ["-"] ["_"] memberManifest.package.name);
      stubs =
        lib.optional (libPath != null) {
          file = libPath;
          main = false;
        }
        ++ map (file: {
          inherit file;
          main = true;
        })
        mainFiles
        ++ withHarness "test" "tests"
        ++ withHarness "bench" "benches";
    })
    memberPaths;

  manifests = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions (
      [
        (root + "/.cargo")
        (root + "/Cargo.toml")
        (root + "/Cargo.lock")
      ]
      ++ map (path: root + "/${path}/Cargo.toml") memberPaths
    );
  };

  writeStub = member: stub: let
    file = lib.escapeShellArg "${member.path}/${stub.file}";
  in ''
    mkdir -p "$(dirname ${file})"
    ${
      if stub.main
      then "echo 'fn main() {}' > ${file}"
      else ": > ${file}"
    }
  '';
in {
  source = runCommand "cargo-dependencies-source" {} ''
    cp -r ${manifests} "$out"
    chmod -R u+w "$out"
    cd "$out"
    ${lib.concatMapStrings (member: lib.concatMapStrings (writeStub member) member.stubs) members}
  '';

  # Drops the stubbed members from a target directory, so the real build compiles them instead of
  # finding them fresh, and their outputs never sit next to the real ones. Cargo's fine-grain-locking
  # keeps each package under build/<name>/, the older layout spreads it over .fingerprint/ and deps/.
  removeMembers = targetDir:
    lib.concatMapStrings (member: let
      name = lib.escapeShellArg member.name;
      libName = lib.escapeShellArg member.libName;
    in ''
      rm -rf ${targetDir}/*/build/${name} ${targetDir}/*/build/${name}-* \
        ${targetDir}/*/.fingerprint/${name}-* ${targetDir}/*/deps/${libName}-* \
        ${targetDir}/*/deps/lib${libName}-* ${targetDir}/*/deps/lib${libName}.* \
        ${targetDir}/doc/${libName}
    '')
    members
    + ''
      rm -rf ${targetDir}/*/incremental ${targetDir}/*/examples
      # Cargo copies only workspace members' binaries and libraries to the top of a profile
      # directory, so these are all stubs.
      find ${targetDir}/* -mindepth 1 -maxdepth 1 -type f ! -name '.*' -delete
    '';

  # Cargo rebuilds a crate when a dependency's output is newer than its own. The store resets every
  # mtime and a plain copy stamps files in copy order, so every file gets the same time instead.
  restore = artifacts: targetDir: ''
    cp -r ${artifacts} ${targetDir}
    chmod -R u+w ${targetDir}
    restoredAt="$(date +%s)"
    find ${targetDir} -exec touch -h -d "@$restoredAt" {} +
  '';
}
