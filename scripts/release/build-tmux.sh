#!/usr/bin/env bash
set -euo pipefail

TMUX_VERSION=3.8
TMUX_URL=https://github.com/tmux/tmux/releases/download/3.8/tmux-3.8.tar.gz
TMUX_SHA256=e79c699c7e949dccd0a4a125e17b8d1261e311b16979dbc8eb34542f3966d82e
LIBEVENT_VERSION=2.1.13-stable
LIBEVENT_URL=https://github.com/libevent/libevent/releases/download/release-2.1.13-stable/libevent-2.1.13-stable.tar.gz
LIBEVENT_SHA256=f7e9383b8c0baa81b687e5b5eecc01beefaf1b19b64151d95ed61647fe7a315c
UTF8PROC_VERSION=2.12.0
UTF8PROC_URL=https://github.com/JuliaStrings/utf8proc/archive/refs/tags/v2.12.0.tar.gz
UTF8PROC_SHA256=f564011d38b2888d583d510b08e69ffa15aa117155db1b9b49ef1dfe1fa25111
NCURSES_VERSION=6.6
NCURSES_URL=https://invisible-mirror.net/archives/ncurses/ncurses-6.6.tar.gz
NCURSES_SHA256=355b4cbbed880b0381a04c46617b7656e362585d52e9cf84a67e2009b749ff11
JEMALLOC_VERSION=5.4.0
JEMALLOC_URL=https://github.com/jemalloc/jemalloc/releases/download/5.4.0/jemalloc-5.4.0.tar.bz2
JEMALLOC_SHA256=200776fac271093e7c2f21edd6d62657ecd2be578d9328633f2a86bfa6ef4f1d
MACOS_MIN_AARCH64=11.0
MACOS_MIN_X86_64=10.13
MACOS_WEAK_ALLOWED="_malloc_default_purgeable_zone ___darwin_check_fd_set_overflow"
SOCKET_PATH_MAX=103
USAGE="usage: build-tmux.sh --target <aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu> --work <new folder> --out <new file>"

refuse() {
  echo "$1" >&2
  echo "$USAGE" >&2
  exit 2
}

fail() {
  echo "build-tmux: $1" >&2
  exit 1
}

absolute() {
  case "$1" in
    /*) echo "$1" ;;
    *) echo "$PWD/$1" ;;
  esac
}

sha256_of() {
  if command -v sha256sum > /dev/null; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

fetch() {
  local name="$1" url="$2" sha256="$3"
  local file="$work/downloads/$name.tarball"
  curl --fail --silent --show-error --location --max-redirs 5 --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --output "$file" "$url"
  if [ "$(sha256_of "$file")" != "$sha256" ]; then
    fail "build_tmux_checksum_mismatch: $name"
  fi
  mkdir "$work/sources/$name"
  tar -xf "$file" -C "$work/sources/$name" --strip-components 1
}

build_libevent() {
  local tree="$1"
  (cd "$tree/libevent" && ./configure --prefix="$prefix" ${configure_args[@]+"${configure_args[@]}"} --disable-shared --enable-static --disable-openssl --disable-samples --disable-libevent-regress --disable-debug-mode > "$tree/libevent.log" 2>&1 && make -j"$jobs" >> "$tree/libevent.log" 2>&1 && make install >> "$tree/libevent.log" 2>&1) || fail "build_tmux_libevent_failed: $tree/libevent.log"
}

build_jemalloc() {
  local tree="$1"
  (cd "$tree/jemalloc" && ./configure --prefix="$prefix" ${configure_args[@]+"${configure_args[@]}"} --disable-cxx --disable-debug --with-lg-page="$page" > "$tree/jemalloc.log" 2>&1 && make -j"$jobs" build_lib_static >> "$tree/jemalloc.log" 2>&1 && make install_include install_lib_static >> "$tree/jemalloc.log" 2>&1) || fail "build_tmux_jemalloc_failed: $tree/jemalloc.log"
  test -f "$prefix/lib/libjemalloc.a" || fail "build_tmux_jemalloc_failed: no static library"
}

build_utf8proc() {
  local tree="$1"
  (cd "$tree/utf8proc" && make -j"$jobs" libutf8proc.a CC="$CC" CFLAGS="$CFLAGS" > "$tree/utf8proc.log" 2>&1) || fail "build_tmux_utf8proc_failed: $tree/utf8proc.log"
  mkdir -p "$prefix/lib" "$prefix/include"
  cp "$tree/utf8proc/libutf8proc.a" "$prefix/lib/libutf8proc.a"
  cp "$tree/utf8proc/utf8proc.h" "$prefix/include/utf8proc.h"
}

build_ncurses() {
  local tree="$1"
  (cd "$tree/ncurses" && ./configure --prefix="$prefix" --without-shared --with-normal --without-debug --without-ada --without-cxx --without-cxx-binding --without-manpages --without-progs --without-tests --disable-db-install --enable-overwrite --with-default-terminfo-dir=/usr/share/terminfo --with-terminfo-dirs=/etc/terminfo:/lib/terminfo:/usr/share/terminfo > "$tree/ncurses.log" 2>&1 && make -j"$jobs" >> "$tree/ncurses.log" 2>&1 && make install.libs >> "$tree/ncurses.log" 2>&1) || fail "build_tmux_ncurses_failed: $tree/ncurses.log"
}

build_tmux() {
  local tree="$1"
  shift
  (cd "$tree/tmux" && ./configure --prefix="$prefix" ${configure_args[@]+"${configure_args[@]}"} --enable-utf8proc "$@" CPPFLAGS="-I$prefix/include" LDFLAGS="$LDFLAGS -L$prefix/lib" LIBEVENT_CORE_CFLAGS="-I$prefix/include" LIBEVENT_CORE_LIBS="-L$prefix/lib -levent_core" LIBUTF8PROC_CFLAGS="-I$prefix/include" LIBUTF8PROC_LIBS="-L$prefix/lib -lutf8proc" > "$tree/tmux.log" 2>&1 && make -j"$jobs" >> "$tree/tmux.log" 2>&1) || fail "build_tmux_tmux_failed: $tree/tmux.log"
  test -f "$tree/tmux/tmux" || fail "build_tmux_tmux_failed: no program"
}

check_version() {
  local version
  version="$("$@" -V)" || fail "build_tmux_version_wrong: tmux -V failed"
  if [ "$version" != "tmux $TMUX_VERSION" ]; then
    fail "build_tmux_version_wrong: $version"
  fi
}

check_jemalloc() {
  local binary="$1" symbols
  symbols="$(nm -U "$binary")" || fail "build_tmux_check_failed: nm -U"
  if ! grep -q -E ' _(je_)?zone_register$' <<< "$symbols"; then
    fail "build_tmux_jemalloc_missing: no jemalloc malloc zone in tmux"
  fi
  if ! grep -q -E '^#define HAVE_JEMALLOC' "$tree/tmux/config.h" 2> /dev/null && ! grep -q -- '-DHAVE_JEMALLOC=1' "$tree/tmux/Makefile"; then
    fail "build_tmux_jemalloc_missing: tmux was configured without jemalloc"
  fi
}

check_weak_imports() {
  local binary="$1" minimum="$2" symbols weak
  symbols="$(nm -m "$binary")" || fail "build_tmux_check_failed: nm -m"
  weak="$(awk -v allowed="$MACOS_WEAK_ALLOWED" 'BEGIN { count = split(allowed, names, " "); for (i = 1; i <= count; i++) ok[names[i]] = 1 } /\(undefined\) weak external/ { for (i = 1; i < NF; i++) if ($i == "external" && !($(i + 1) in ok)) print $(i + 1) }' <<< "$symbols")"
  if [ -n "$weak" ]; then
    fail "build_tmux_weak_import: ${weak//$'\n'/ } is newer than macOS $minimum, so it is NULL there"
  fi
}

check_start() {
  local socket="$work/s"
  if ! env -u TMUX "$@" -S "$socket" -f /dev/null new-session -d -s check; then
    env -u TMUX "$@" -S "$socket" kill-server 2> /dev/null || true
    fail "build_tmux_start_failed: tmux could not start a server"
  fi
  if ! env -u TMUX "$@" -S "$socket" has-session -t check; then
    env -u TMUX "$@" -S "$socket" kill-server 2> /dev/null || true
    fail "build_tmux_start_failed: the server lost its session"
  fi
  env -u TMUX "$@" -S "$socket" kill-server || fail "build_tmux_start_failed: the server did not stop"
}

check_macos() {
  local binary="$1" arch="$2" minimum="$3" archs libraries foreign load
  archs="$(lipo -archs "$binary")" || fail "build_tmux_check_failed: lipo -archs"
  if [ "$archs" != "$arch" ]; then
    fail "build_tmux_arch_wrong: $archs"
  fi
  libraries="$(otool -L "$binary")" || fail "build_tmux_check_failed: otool -L"
  foreign="$(awk 'NR > 1 && $1 !~ /^\/(usr\/lib|System\/Library)\// { print $1 }' <<< "$libraries")"
  if [ -n "$foreign" ]; then
    fail "build_tmux_links_outside_the_system: $foreign"
  fi
  load="$(otool -l "$binary")" || fail "build_tmux_check_failed: otool -l"
  if ! grep -q -E "^ *(minos|version) $minimum\$" <<< "$load"; then
    fail "build_tmux_minimum_wrong: not macOS $minimum"
  fi
  check_weak_imports "$binary" "$minimum"
  if /usr/bin/arch "-$arch" /usr/bin/true 2> /dev/null; then
    check_version /usr/bin/arch "-$arch" "$binary"
    check_start /usr/bin/arch "-$arch" "$binary"
  else
    echo "build-tmux: $arch cannot run on this Mac, so tmux -V and the server start were not checked" >&2
  fi
}

check_linux() {
  local binary="$1" program dynamic
  program="$(readelf -l "$binary")" || fail "build_tmux_check_failed: readelf -l"
  if grep -q 'program interpreter' <<< "$program"; then
    fail "build_tmux_not_static: it names a program interpreter"
  fi
  dynamic="$(readelf -d "$binary")" || fail "build_tmux_check_failed: readelf -d"
  if grep -q '(NEEDED)' <<< "$dynamic"; then
    fail "build_tmux_not_static: it needs shared libraries"
  fi
  check_version "$binary"
  check_start "$binary"
}

target=""
work=""
out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --target | --work | --out)
      if [ "$#" -lt 2 ]; then
        refuse build_tmux_arguments_rejected
      fi
      case "$1" in
        --target) target="$2" ;;
        --work) work="$2" ;;
        --out) out="$2" ;;
      esac
      shift 2
      ;;
    *) refuse build_tmux_arguments_rejected ;;
  esac
done
if [ -z "$target" ] || [ -z "$work" ] || [ -z "$out" ]; then
  refuse build_tmux_arguments_missing
fi
case "$target" in
  aarch64-apple-darwin) os=macos arch=arm64 minimum="$MACOS_MIN_AARCH64" page=14 ;;
  x86_64-apple-darwin) os=macos arch=x86_64 minimum="$MACOS_MIN_X86_64" page=12 ;;
  x86_64-unknown-linux-gnu) os=linux arch=x86_64 minimum="" page="" ;;
  aarch64-unknown-linux-gnu) os=linux arch=aarch64 minimum="" page="" ;;
  *) refuse build_tmux_target_rejected ;;
esac
work="$(absolute "$work")"
out="$(absolute "$out")"
if [ -e "$work" ] || [ -L "$work" ]; then
  refuse build_tmux_work_exists
fi
if [ -e "$out" ] || [ -L "$out" ]; then
  refuse build_tmux_out_exists
fi
if [ "$(( ${#work} + 2 ))" -gt "$SOCKET_PATH_MAX" ]; then
  refuse build_tmux_work_too_long
fi

export PATH=/usr/bin:/bin:/usr/sbin:/sbin
unset CPATH C_INCLUDE_PATH LIBRARY_PATH PKG_CONFIG_PATH CPPFLAGS LIBS MAKEFLAGS
case "$os:$(uname -s)" in
  macos:Darwin | linux:Linux) ;;
  *) fail "build_tmux_host_wrong: $target builds on $os only" ;;
esac
tools=(curl make tar cut awk grep)
if [ "$os" = macos ]; then
  tools+=(clang lipo otool nm yacc shasum bzip2)
else
  tools+=(musl-gcc readelf)
  if [ "$(uname -m)" != "$arch" ]; then
    fail "build_tmux_host_wrong: $target builds only on a $arch Linux host"
  fi
fi
for tool in "${tools[@]}"; do
  command -v "$tool" > /dev/null || fail "build_tmux_tool_missing: $tool"
done
if [ "$os" = linux ] && ! command -v yacc > /dev/null && ! command -v bison > /dev/null; then
  fail "build_tmux_tool_missing: yacc or bison"
fi

jobs="$(getconf _NPROCESSORS_ONLN)"
mkdir -p "$work/downloads" "$work/sources"
fetch tmux "$TMUX_URL" "$TMUX_SHA256"
fetch libevent "$LIBEVENT_URL" "$LIBEVENT_SHA256"
fetch utf8proc "$UTF8PROC_URL" "$UTF8PROC_SHA256"
if [ "$os" = linux ]; then
  fetch ncurses "$NCURSES_URL" "$NCURSES_SHA256"
else
  fetch jemalloc "$JEMALLOC_URL" "$JEMALLOC_SHA256"
fi

tree="$work/sources"
prefix="$work/prefix"
export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
if [ "$os" = macos ]; then
  export CC=clang
  export MACOSX_DEPLOYMENT_TARGET="$minimum"
  export CFLAGS="-arch $arch -mmacosx-version-min=$minimum -O2"
  export LDFLAGS="-arch $arch -mmacosx-version-min=$minimum"
  configure_args=("--host=${target}" "--build=$(uname -m | sed 's/^arm64$/aarch64/')-apple-darwin" ac_cv_func_pipe2=no)
  build_jemalloc "$tree"
  build_libevent "$tree"
  build_utf8proc "$tree"
  build_tmux "$tree" --enable-jemalloc JEMALLOC_CFLAGS="-I$prefix/include" JEMALLOC_LIBS="-Wl,-force_load,$prefix/lib/libjemalloc.a"
  check_macos "$tree/tmux/tmux" "$arch" "$minimum"
  check_jemalloc "$tree/tmux/tmux"
else
  export CC=musl-gcc
  export CFLAGS="-O2"
  export LDFLAGS="-static"
  configure_args=()
  build_ncurses "$tree"
  build_libevent "$tree"
  build_utf8proc "$tree"
  build_tmux "$tree" --enable-static
  check_linux "$tree/tmux/tmux"
fi

cp "$tree/tmux/tmux" "$out"
chmod 0755 "$out"
echo "$out"
