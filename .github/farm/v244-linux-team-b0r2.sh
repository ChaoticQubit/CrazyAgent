set -uo pipefail
stage=${1:?stage: facts, build-helper, build-supervisor, build-lib, build-box, build-mounts, supervisor, lib, box or mounts}
out="$RUNNER_TEMP/b0r2-$stage-restricted.txt"
base=238ab31c7
failed=0
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
host_state() {
  local label=$1
  echo "-- host state $label $(date -u +%H:%M:%S)" | tee -a "$out"
  free -m | tee -a "$out"
  echo "runner processes $(pgrep -u runner -c || true), sbtest processes $(pgrep -u sbtest -c || true)" | tee -a "$out"
  sudo dmesg -T 2>/dev/null | grep -iE 'oom|killed process|segfault|apparmor="DENIED"|landlock' | tail -20 | tee -a "$out" || true
  echo "systemd-oomd $(systemctl is-active systemd-oomd 2>/dev/null || true), memory pressure $(cat /proc/pressure/memory 2>/dev/null | tr '\n' ' ')" | tee -a "$out"
  sudo journalctl -u systemd-oomd --no-pager 2>/dev/null | grep -iE 'kill|pressure|swap' | tail -10 | tee -a "$out" || true
}
whole() {
  local label=$1 file=$2 rc=$3
  local reds counts
  grep -E '^test result:' "$file" | tee -a "$out" || true
  counts=$(grep -E '^test result:' "$file" | awk '{for (i = 1; i <= NF; i++) { if ($(i+1) ~ /^(passed|failed|ignored|filtered)/) { k = $(i+1); sub(/[;,]/, "", k); n[k] += $i } } b++} END {printf "binaries %d passed %d failed %d ignored %d filtered %d", b, n["passed"], n["failed"], n["ignored"], n["filtered"]}')
  row "$label" "record, rc $rc" "$counts"
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ' || true)
  for name in $reds; do
    echo "-- red $name" | tee -a "$out"
    block "$name" "$file" | grep -A6 'panicked at' | head -8 | tee -a "$out"
  done
  if ! grep -qE '^test result:' "$file"; then
    row "$label ended" "a test result" "NOT: rc $rc, last line: $(tail -c 300 "$file" | tr '\n' ' ')"
    failed=1
  else
    row "$label reds" record "${reds:-none}"
  fi
}
as_test_user() {
  sudo systemd-run --quiet --wait --pipe --collect --uid=sbtest --gid=sbtest \
    -p MemoryMax=8G -p TasksMax=4096 -p RuntimeMaxSec=1500 \
    --working-directory="$GITHUB_WORKSPACE/crates/crazyagent" \
    --setenv=HOME=/home/sbtest --setenv=PATH=/usr/bin:/bin --setenv=LANG=C.UTF-8 \
    --setenv=CARGO_MANIFEST_DIR="$GITHUB_WORKSPACE/crates/crazyagent" \
    --setenv=CARGO_TARGET_TMPDIR="$GITHUB_WORKSPACE/target/tmp" \
    -- "$@"
}
executables() {
  sed -n 's/.*Executable .* (\(.*\))$/\1/p' "$1" | while read -r path; do realpath "$path"; done
}
case "$stage" in
facts)
  code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
  [ "$code_tip" = "$base" ] || { row "code tip" "$base" "NOT: $code_tip"; exit 1; }
  row "code tip" "$base" "$code_tip"
  row "kernel" record "$(uname -r)"
  row "arch" record "$(uname -m)"
  row "lsm" record "$(cat /sys/kernel/security/lsm 2>/dev/null || echo unread)"
  abi=$(python3 -c 'import ctypes; l = ctypes.CDLL(None, use_errno=True); print(l.syscall(444, None, ctypes.c_size_t(0), ctypes.c_uint32(1)))' 2>&1)
  row "landlock abi" record "$abi"
  row "apparmor_restrict_unprivileged_userns" record "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo absent)"
  bwrap=$(command -v bwrap || true)
  row "bwrap" record "${bwrap:-missing} $("${bwrap:-/bin/false}" --version 2>&1 | head -1)"
  if [ -n "$bwrap" ] && sudo -u sbtest "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc -- /bin/true 2>"$RUNNER_TEMP/nest-restricted.txt"; then
    row "bubblewrap nests for sbtest" record yes
  else
    row "bubblewrap nests for sbtest" record "no: $(head -c 300 "$RUNNER_TEMP/nest-restricted.txt" | tr '\n' ' ')"
  fi
  row "test user" record "$(id sbtest)"
  host_state facts
  ;;
build-helper)
  host_state "before the helper build"
  cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/build-helper-restricted.txt" 2>&1
  if [ -x target/debug/crazyagent-landlock ]; then
    row "helper built" built "built $(sha256sum target/debug/crazyagent-landlock | cut -c1-12)"
  else
    row "helper built" built "NOT: missing"
    tail -30 "$RUNNER_TEMP/build-helper-restricted.txt" | tee -a "$out"
    failed=1
  fi
  host_state "after the helper build"
  ;;
build-supervisor | build-lib | build-box | build-mounts)
  name=${stage#build-}
  case "$name" in
  supervisor) target=(-p crazyagent-linux-supervisor) ;;
  lib) target=(-p crazyagent --lib) ;;
  box) target=(-p crazyagent --test linux_sandbox) ;;
  mounts) target=(-p crazyagent --test linux_hidden_mounts) ;;
  esac
  host_state "before the $name build"
  started=$(date +%s)
  cargo test "${target[@]}" --locked --no-run > "$RUNNER_TEMP/build-$name-restricted.txt" 2>&1
  rc=$?
  executables "$RUNNER_TEMP/build-$name-restricted.txt" > "$RUNNER_TEMP/exe-$name"
  row "test binaries $name" "at least 1, rc 0" "rc $rc, $(( $(date +%s) - started )) s, $(wc -l < "$RUNNER_TEMP/exe-$name" | tr -d ' ') $(xargs -n1 basename < "$RUNNER_TEMP/exe-$name" | tr '\n' ' ')"
  if [ "$rc" -ne 0 ] || [ ! -s "$RUNNER_TEMP/exe-$name" ]; then
    tail -30 "$RUNNER_TEMP/build-$name-restricted.txt" | tee -a "$out"
    failed=1
  fi
  host_state "after the $name build"
  if [ "$name" = mounts ]; then
    mkdir -p target/tmp
    chmod 1777 target/tmp
    sudo chmod o+x /home/runner /home/runner/work "$(dirname "$GITHUB_WORKSPACE")"
    chmod -R o+rX "$GITHUB_WORKSPACE"
    if sudo -u sbtest test -x "$GITHUB_WORKSPACE/target/debug/crazyagent-landlock"; then
      row "sbtest reaches the helper" yes yes
    else
      row "sbtest reaches the helper" yes NOT
      failed=1
    fi
  fi
  ;;
supervisor|lib|box|mounts)
  host_state "before $stage"
  rc_all=0
  while read -r exe; do
    log="$RUNNER_TEMP/run-$stage-$(basename "$exe")-restricted.txt"
    case "$stage" in
      lib) as_test_user "$exe" sandbox:: master_host_launch --test-threads=1 > "$log" 2>&1 ;;
      mounts) as_test_user "$exe" --ignored --test-threads=1 > "$log" 2>&1 ;;
      *) as_test_user "$exe" --test-threads=1 > "$log" 2>&1 ;;
    esac
    rc=$?
    [ "$rc" -eq 0 ] || rc_all=$rc
    whole "$stage $(basename "$exe")" "$log" "$rc"
  done < "$RUNNER_TEMP/exe-$stage"
  host_state "after $stage"
  row "$stage" "rc 0" "rc $rc_all"
  ;;
esac
echo "B0r2 $stage failed=$failed" | tee -a "$out"
exit "$failed"
