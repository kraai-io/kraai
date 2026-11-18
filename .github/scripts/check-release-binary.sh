#!/usr/bin/env bash
# check-release-binary.sh <binary>: checks target/$TARGET/release/<binary>$EXE before it ships.
# Native builds must print KRAAI_VERSION for --version. Every build may link only libraries the
# operating system provides, and Linux builds may need no newer glibc than GLIBC_FLOOR.
set -euo pipefail

bin=$1
path="target/$TARGET/release/$bin${EXE:-}"
fail() {
  echo "::error::$bin: $*"
  exit 1
}
[ -f "$path" ] || fail "$path does not exist"

if [ -z "${OBJDUMP:-}" ]; then
  sysroot=$(rustc --print sysroot)
  if command -v cygpath > /dev/null; then
    sysroot=$(cygpath -u "$sysroot")
  fi
  OBJDUMP="$sysroot/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-objdump"
fi

if [ "${CROSS:-}" != true ]; then
  actual=$("$path" --version)
  [ "$actual" = "$bin $KRAAI_VERSION" ] || fail "--version printed '$actual', expected '$bin $KRAAI_VERSION'"
fi

case "$TARGET" in
  *-linux-gnu)
    libraries=$("$OBJDUMP" -p "$path" | awk '$1 == "NEEDED" { print $2 }')
    unexpected=$(grep -vxE 'libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|ld-linux-(x86-64|aarch64)\.so\.[0-9]+' <<< "$libraries" || true)
    glibc=$("$OBJDUMP" -T "$path" | grep -o 'GLIBC_[0-9.]*[0-9]' | sed 's/^GLIBC_//' | sort -uV | tail -1)
    [ -n "$glibc" ] || fail "has no GLIBC symbol versions"
    if [ "$(printf '%s\n%s\n' "$glibc" "$GLIBC_FLOOR" | sort -V | tail -1)" != "$GLIBC_FLOOR" ]; then
      fail "needs GLIBC_$glibc, newer than the GLIBC_$GLIBC_FLOOR floor"
    fi
    echo "$bin needs GLIBC_$glibc (floor $GLIBC_FLOOR)"
    ;;
  *-apple-darwin)
    libraries=$("$OBJDUMP" --macho --dylibs-used "$path" | tail -n +2 | awk '{ print $1 }')
    unexpected=$(grep -vE '^/(usr/lib|System/Library)/' <<< "$libraries" || true)
    ;;
  *-windows-msvc)
    libraries=$("$OBJDUMP" -p "$path" | sed -n 's/^[[:space:]]*DLL Name: //p')
    unexpected=$(grep -iE '^(vcruntime|msvcp|concrt|vccorlib)' <<< "$libraries" || true)
    ;;
  *) fail "no library check for $TARGET" ;;
esac
[ -z "$unexpected" ] || fail "links libraries the operating system does not provide: $(paste -sd ' ' - <<< "$unexpected")"
echo "$bin links: $(paste -sd ' ' - <<< "$libraries")"
