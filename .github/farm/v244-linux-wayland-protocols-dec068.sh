#!/usr/bin/env bash
set +e -uo pipefail

out="$RUNNER_TEMP/v244-linux-wayland-protocols-dec068.txt"
: > "$out"
log() { printf '%s\n' "$*" | tee -a "$out"; }
crate=wayland-protocols
version=0.32.13
lines_max=4000

log "DEC-068 inspection of $crate $version at $INSPECT_SHA (farm commit $(git rev-parse --short HEAD)), $(cargo -V)"
log "== Cargo.lock entry"
awk -v n="$crate" -v v="$version" '
  function flush() { if (name && ver) { printf "%s", block; done = 1 } }
  /^\[\[package\]\]/ { flush(); if (done) exit; block = ""; name = 0; ver = 0 }
  { block = block $0 "\n" }
  $0 == "name = \"" n "\"" { name = 1 }
  $0 == "version = \"" v "\"" { ver = 1 }
  END { if (!done) flush() }
' Cargo.lock > "$RUNNER_TEMP/lock-entry.txt"
tee -a "$out" < "$RUNNER_TEMP/lock-entry.txt"
lock_sum=$(sed -n 's/^checksum = "\([0-9a-f]\{64\}\)"$/\1/p' "$RUNNER_TEMP/lock-entry.txt")
lock_source=$(sed -n 's/^source = "\(.*\)"$/\1/p' "$RUNNER_TEMP/lock-entry.txt")
grep -qx "version = \"$version\"" "$RUNNER_TEMP/lock-entry.txt"
lock_version=$?

log "== cargo fetch --locked from the registry, no cache restored"
cargo fetch --locked > "$RUNNER_TEMP/fetch.txt" 2>&1
fetched=$?
tail -n 5 "$RUNNER_TEMP/fetch.txt" | tee -a "$out"
home="${CARGO_HOME:-$HOME/.cargo}"
archive=$(find "$home/registry/cache" -name "$crate-$version.crate" 2>/dev/null | head -n 1)
dir=$(find "$home/registry/src" -maxdepth 2 -type d -name "$crate-$version" 2>/dev/null | head -n 1)
log "archive: ${archive#$home/}"
log "source dir: ${dir#$home/}"
archive_sum=na
[ -n "$archive" ] && archive_sum=$(sha256sum "$archive" | cut -d' ' -f1)
log "archive sha256 $archive_sum"
log "lock checksum  $lock_sum"
[ -n "$lock_sum" ] && [ "$archive_sum" = "$lock_sum" ]
sum_ok=$?
[ "$lock_source" = "registry+https://github.com/rust-lang/crates.io-index" ]
source_ok=$?

listed=1
build_rs=na
build_key=na
risk=na
if [ -n "$dir" ]; then
  cd "$dir" || exit 2
  log "== .cargo_vcs_info.json"
  cat .cargo_vcs_info.json 2>&1 | tee -a "$out"
  log "== file list: bytes, sha256, path"
  find . -type f | LC_ALL=C sort | while read -r f; do
    printf '%s %s %s\n' "$(stat -c %s "$f")" "$(sha256sum "$f" | cut -d' ' -f1)" "$f"
  done | tee -a "$out"
  listed=${PIPESTATUS[0]}
  log "files $(find . -type f | wc -l), bytes $(du -sb . | cut -f1)"
  log "by extension:"
  find . -type f | sed -E 's/.*\.([A-Za-z0-9]+)$/\1/; t; s/.*/(none)/' | sort | uniq -c | tee -a "$out"
  log "== Cargo.toml"
  tee -a "$out" < Cargo.toml
  if [ -f Cargo.toml.orig ]; then
    log "== Cargo.toml.orig"
    tee -a "$out" < Cargo.toml.orig
  fi
  build_key=$(grep -cE '^\s*build\s*=' Cargo.toml)
  log "build key in Cargo.toml: $build_key"
  rs_build=$(find . -name build.rs -type f | LC_ALL=C sort)
  if [ -n "$rs_build" ]; then
    build_rs=$(printf '%s\n' "$rs_build" | wc -l)
    for f in $rs_build; do
      log "== $f"
      tee -a "$out" < "$f"
    done
  else
    build_rs=0
    log "== build.rs: none in the crate"
  fi
  log "== risk grep over every .rs file (count per pattern)"
  risk=0
  for pattern in 'unsafe' 'std::process' 'Command::new' 'std::net' 'std::fs' 'env!' 'option_env!' 'include!' 'include_str!' 'include_bytes!' 'extern "C"' '#[link' 'proc_macro' 'std::env'; do
    n=$(grep -rF --include='*.rs' -- "$pattern" . | wc -l)
    log "$n  $pattern"
    risk=$((risk + n))
  done
  log "== every .rs file"
  total=$(find . -name '*.rs' -type f -exec cat {} + | wc -l)
  log "rs lines $total (printed in full when at most $lines_max)"
  if [ "$total" -le "$lines_max" ]; then
    find . -name '*.rs' -type f | LC_ALL=C sort | while read -r f; do
      log "---- $f"
      tee -a "$out" < "$f"
    done
  else
    log "::warning::rs files exceed $lines_max lines; only src/lib.rs printed"
    tee -a "$out" < src/lib.rs
  fi
  cd "$GITHUB_WORKSPACE" || exit 2
fi

log "== who depends on it (cargo tree -i, Linux target)"
cargo tree --locked --offline -p crazyagent-desktop --target x86_64-unknown-linux-gnu -i "$crate@$version" -e features 2>&1 | tee -a "$out"
inv=${PIPESTATUS[0]}
log "== what it pulls in (normal and build edges, Linux target)"
cargo tree --locked --offline -p "$crate@$version" --target x86_64-unknown-linux-gnu -e normal,build 2>&1 | tee -a "$out"
fwd=${PIPESTATUS[0]}

{
  printf '| wayland-protocols %s DEC-068 | result |\n| --- | --- |\n' "$version"
  printf '| lock entry names version %s (0 = yes) | %s |\n' "$version" "$lock_version"
  printf '| lock source is crates.io (0 = yes) | %s |\n' "$source_ok"
  printf '| cargo fetch --locked | %s |\n' "$fetched"
  printf '| fetched .crate sha256 equals the lock checksum (0 = yes) | %s |\n' "$sum_ok"
  printf '| file list printed | %s |\n' "$listed"
  printf '| build.rs files / build key in Cargo.toml | %s / %s |\n' "$build_rs" "$build_key"
  printf '| risk grep hits over .rs (see log) | %s |\n' "$risk"
  printf '| cargo tree -i / forward | %s / %s |\n' "$inv" "$fwd"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$lock_version" = 0 ] && [ "$source_ok" = 0 ] && [ "$fetched" = 0 ] && [ "$sum_ok" = 0 ] \
  && [ "$listed" = 0 ] && [ "$inv" = 0 ] && [ "$fwd" = 0 ]
