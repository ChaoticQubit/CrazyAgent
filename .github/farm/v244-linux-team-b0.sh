set -uo pipefail
kind=${1:?host kind: default or restricted}
out="$RUNNER_TEMP/v244-linux-team-b0-$kind.txt"
base=238ab31c7
helper=target/debug/crazyagent-landlock
failed=0
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$base" ]; then
  row "code tip" "$base" "NOT: $code_tip"
  exit 1
fi
row "code tip" "$base" "$code_tip"
row "host kind" "$kind" "$kind"
row "kernel" record "$(uname -r)"
row "arch" record "$(uname -m)"
row "lsm" record "$(cat /sys/kernel/security/lsm 2>/dev/null || echo unread)"
abi=$(python3 -c 'import ctypes; l = ctypes.CDLL(None, use_errno=True); print(l.syscall(444, None, ctypes.c_size_t(0), ctypes.c_uint32(1)))' 2>&1)
row "landlock abi" record "$abi"
row "apparmor_restrict_unprivileged_userns" record "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo absent)"
bwrap=$(command -v bwrap || true)
row "bwrap" record "${bwrap:-missing} $("${bwrap:-/bin/false}" --version 2>&1 | head -1)"
if [ -n "$bwrap" ] && "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc -- /bin/true 2>"$RUNNER_TEMP/nest-$kind.txt"; then
  row "bubblewrap nests" record yes
else
  row "bubblewrap nests" record "no: $(head -c 300 "$RUNNER_TEMP/nest-$kind.txt" | tr '\n' ' ')"
fi
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
whole() {
  local label=$1 file=$2
  local reds counts
  grep -E '^test result:' "$file" | tee -a "$out" || true
  counts=$(grep -E '^test result:' "$file" | awk '{for (i = 1; i <= NF; i++) { if ($(i+1) ~ /^(passed|failed|ignored|filtered)/) { k = $(i+1); sub(/[;,]/, "", k); n[k] += $i } } b++} END {printf "binaries %d passed %d failed %d ignored %d filtered %d", b, n["passed"], n["failed"], n["ignored"], n["filtered"]}')
  row "$label" record "$counts"
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ' || true)
  for name in $reds; do
    echo "-- red $name" | tee -a "$out"
    block "$name" "$file" | grep -A6 'panicked at' | head -8 | tee -a "$out"
  done
  if [ -n "$reds" ]; then
    row "$label reds" none "NOT: $reds"
    [ "$kind" = default ] && failed=1
  elif ! grep -qE '^test result: ok' "$file"; then
    row "$label reds" none "NOT: no ok result"
    tail -30 "$file" | tee -a "$out"
    [ "$kind" = default ] && failed=1
  else
    row "$label reds" none none
  fi
}
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-$kind.txt" 2>&1
tail -2 "$RUNNER_TEMP/helper-$kind.txt"
if [ -x "$helper" ]; then
  row "helper built" built "built $(sha256sum "$helper" | cut -c1-12)"
else
  row "helper built" built "NOT: missing"
  tail -30 "$RUNNER_TEMP/helper-$kind.txt"
  exit 1
fi
cargo test -p crazyagent-linux-supervisor --locked > "$RUNNER_TEMP/supervisor-$kind.txt" 2>&1
whole "supervisor crate" "$RUNNER_TEMP/supervisor-$kind.txt"
exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-lib-$kind.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
tail -2 "$RUNNER_TEMP/build-lib-$kind.txt"
if [ -z "$exe" ] || [ ! -x "$exe" ]; then
  row "lib test binary" built "NOT: none"
  tail -30 "$RUNNER_TEMP/build-lib-$kind.txt"
  exit 1
fi
exe=$(realpath "$exe")
(cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" sandbox:: master_host_launch > "$RUNNER_TEMP/lib-$kind.txt" 2>&1)
whole "lib sandbox:: master_host_launch" "$RUNNER_TEMP/lib-$kind.txt"
cargo test -p crazyagent --test linux_sandbox --locked > "$RUNNER_TEMP/box-$kind.txt" 2>&1
whole "linux_sandbox" "$RUNNER_TEMP/box-$kind.txt"
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/mounts-$kind.txt" 2>&1
whole "linux_hidden_mounts --ignored" "$RUNNER_TEMP/mounts-$kind.txt"
echo "B0 $kind failed=$failed" | tee -a "$out"
exit "$failed"
