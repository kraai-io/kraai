# crate2nix ignores Cargo profiles, so this resolves them the way cargo does and turns them into
# rustc flags. Profiles come from Cargo.toml and .cargo/config.toml, with cargo's built-in
# defaults underneath. Keys this file can't translate throw instead of being dropped.
#
# One difference remains. Cargo applies build-override to build scripts, proc macros, and every
# crate they depend on. Here it applies to build scripts and proc macros only, because a
# dependency of a proc macro is the same derivation as the runtime copy of that crate.
{
  lib,
  manifest,
  config,
  memberNames,
}: let
  builtinBuildOverride = {
    opt-level = 0;
    codegen-units = 256;
    debug = false;
  };
  builtinProfiles = {
    dev = {
      opt-level = 0;
      debug = true;
      strip = "none";
      debug-assertions = true;
      overflow-checks = true;
      lto = false;
      panic = "unwind";
      incremental = true;
      codegen-units = 256;
      rpath = false;
      build-override = builtinBuildOverride;
    };
    release = {
      opt-level = 3;
      debug = false;
      strip = "none";
      debug-assertions = false;
      overflow-checks = false;
      lto = false;
      panic = "unwind";
      incremental = false;
      codegen-units = 16;
      rpath = false;
      build-override = builtinBuildOverride;
    };
    debug.inherits = "dev";
    test.inherits = "dev";
    bench.inherits = "release";
  };
  # Cargo reads profiles from both files and the config file wins.
  userProfiles = lib.recursiveUpdate (manifest.profile or {}) (config.profile or {});

  resolve = name: let
    user = userProfiles.${name} or {};
    parent = user.inherits or builtinProfiles.${name}.inherits or null;
    base =
      if parent != null
      then resolve parent
      else builtinProfiles.${name} or (throw "Cargo profile `${name}` needs `inherits`");
  in
    lib.recursiveUpdate base (removeAttrs user ["inherits"]);
  profiles = lib.genAttrs (lib.attrNames (builtinProfiles // userProfiles)) resolve;

  checkPackageKey = key:
    lib.throwIf (lib.hasInfix ":" key || lib.hasInfix "@" key)
    "nix/cargo-profiles.nix does not translate the versioned package override `${key}`"
    key;

  # Precedence, first match wins: package.<name>, package."*" for non-members, build-override
  # for build units, the profile.
  settingsFor = {
    profile,
    crateName,
    buildUnit,
  }: let
    resolved = profiles.${profile} or (throw "unknown Cargo profile `${profile}`");
    packages = lib.mapAttrs' (key: lib.nameValuePair (checkPackageKey key)) (resolved.package or {});
    isMember = lib.elem crateName memberNames;
  in
    removeAttrs resolved ["package" "build-override"]
    // lib.optionalAttrs buildUnit (resolved.build-override or {})
    // lib.optionalAttrs (!isMember) (packages."*" or {})
    // packages.${crateName} or {};

  onOff = value:
    if value
    then "on"
    else "off";
  debugLevel = value:
    {
      "true" = "2";
      "false" = "0";
      "0" = "0";
      "1" = "1";
      "2" = "2";
      none = "0";
      limited = "1";
      full = "2";
      line-tables-only = "line-tables-only";
      line-directives-only = "line-directives-only";
    }.${
      if lib.isBool value
      then lib.boolToString value
      else toString value
    }
    or (throw "unknown Cargo debug setting `${toString value}`");
  stripLevel = value:
    if value == true
    then "symbols"
    else if value == false
    then "none"
    else value;
  ltoLevel = value:
    if value == true
    then "fat"
    else value;

  flag = name: value: ["-C" "${name}=${value}"];
  translate = {
    opt-level = value: flag "opt-level" (toString value);
    debug = value: flag "debuginfo" (debugLevel value);
    strip = value: lib.optionals (stripLevel value != "none") (flag "strip" (stripLevel value));
    debug-assertions = value: flag "debug-assertions" (onOff value);
    overflow-checks = value: flag "overflow-checks" (onOff value);
    codegen-units = value: flag "codegen-units" (toString value);
    # Cargo's lto = false leaves rustc's default, thin local LTO.
    lto = value: lib.optionals (value != false) (flag "lto" (ltoLevel value));
    rustflags = value: value;
    # Every crate is a fresh rustc invocation in Nix, so there's nothing to reuse.
    incremental = _: [];
    panic = value:
      lib.throwIf (value != "unwind")
      "nix/cargo-profiles.nix does not translate panic = \"${value}\", which cargo only applies to some units"
      [];
    rpath = value: lib.throwIf value "nix/cargo-profiles.nix does not translate rpath = true" [];
  };
in {
  # withLto is false for crates rustc can't LTO, such as proc macros and libraries that also build a
  # non-rlib output. The binary's LTO still covers them through the bitcode in their rlibs.
  rustcOpts = {
    profile,
    crateName,
    buildUnit ? false,
    withLto ? true,
  }: let
    settings = settingsFor {inherit profile crateName buildUnit;};
  in
    lib.concatLists (
      lib.mapAttrsToList (
        key: value:
          if key == "lto" && !withLto
          then []
          else (translate.${key} or (throw "nix/cargo-profiles.nix does not translate profile.${profile}.${key}")) value
      )
      (removeAttrs settings ["rustflags"])
    )
    # Cargo passes profile rustflags after its own flags, so they win.
    ++ settings.rustflags or [];
}
