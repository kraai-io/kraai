#!/usr/bin/env bash
set -euo pipefail

bin=$1
path="target/$TARGET/release/$bin${EXE:-}"
fail() {
  echo "::error::$bin: $*"
  exit 1
}
[ -f "$path" ] || fail "$path does not exist"

if [ -z "${LLVM_TOOLS:-}" ]; then
  sysroot=$(rustc --print sysroot)
  if command -v cygpath > /dev/null; then
    sysroot=$(cygpath -u "$sysroot")
  fi
  LLVM_TOOLS="$sysroot/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
fi

if [ "${CROSS:-}" != true ]; then
  actual=$("$path" --version)
  [ "$actual" = "$bin $KRAAI_VERSION" ] || fail "--version printed '$actual', expected '$bin $KRAAI_VERSION'"
fi

case "$TARGET" in
  *-linux-gnu)
    libraries=$("$LLVM_TOOLS/llvm-objdump" -p "$path" | awk '$1 == "NEEDED" { print $2 }')
    unexpected=$(grep -vxE 'libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|ld-linux-(x86-64|aarch64)\.so\.[0-9]+' <<< "$libraries" || true)
    glibc=$("$LLVM_TOOLS/llvm-objdump" -T "$path" | grep -o 'GLIBC_[0-9.]*[0-9]' | sed 's/^GLIBC_//' | sort -uV | tail -1)
    [ -n "$glibc" ] || fail "has no GLIBC symbol versions"
    if [ "$(printf '%s\n%s\n' "$glibc" "$GLIBC_FLOOR" | sort -V | tail -1)" != "$GLIBC_FLOOR" ]; then
      fail "needs GLIBC_$glibc, newer than the GLIBC_$GLIBC_FLOOR floor"
    fi
    echo "$bin needs GLIBC_$glibc (floor $GLIBC_FLOOR)"
    ;;
  *-apple-darwin)
    libraries=$("$LLVM_TOOLS/llvm-objdump" --macho --dylibs-used "$path" | tail -n +2 | awk '{ print $1 }')
    unexpected=$(grep -vE '^/(usr/lib|System/Library)/' <<< "$libraries" || true)
    ;;
  *-windows-msvc)
    libraries=$("$LLVM_TOOLS/llvm-objdump" -p "$path" | sed -n 's/^[[:space:]]*DLL Name: //p')
    unexpected=$(grep -iE '^(vcruntime|msvcp|concrt|vccorlib)' <<< "$libraries" || true)
    resources=$("$LLVM_TOOLS/llvm-readobj" --coff-resources "$path")
    grep -q "Type: GROUP_ICON" <<< "$resources" || fail "has no embedded icon"
    ;;
  *) fail "no library check for $TARGET" ;;
esac
[ -z "$unexpected" ] || fail "links libraries the operating system does not provide: $(paste -sd ' ' - <<< "$unexpected")"
echo "$bin links: $(paste -sd ' ' - <<< "$libraries")"
