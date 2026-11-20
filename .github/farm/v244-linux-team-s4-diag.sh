set -uo pipefail
tip=4760d4f6f
out="$RUNNER_TEMP/v244-linux-team-s4-diag-linux.txt"
debug="$RUNNER_TEMP/setup-debug"
row="research::runner::tests::an_agent_worktree_is_added_on_linux_inside_the_installed_boundary"
: > "$out"
mkdir -p "$debug"
say() { printf '%s\n' "$*" | tee -a "$out"; }
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
[ "$code_tip" = "$tip" ] || { say "code tip NOT: $code_tip"; exit 1; }
say "code tip $tip; $(uname -srm); $(git --version); landlock abi $(cat /sys/kernel/security/landlock/abi_version 2>/dev/null || echo unknown)"
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-linux.txt" 2>&1 || { say "helper build NOT"; tail -20 "$RUNNER_TEMP/helper-linux.txt" | tee -a "$out"; exit 1; }
exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-lib-linux.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
[ -n "$exe" ] && [ -x "$exe" ] || { say "lib test binary NOT"; grep -E -A8 '^error' "$RUNNER_TEMP/build-lib-linux.txt" | head -30 | tee -a "$out"; exit 1; }
exe=$(realpath "$exe")
(cd crates/crazyagent && CRAZYAGENT_SETUP_DEBUG="$debug" CARGO_MANIFEST_DIR="$PWD" "$exe" "$row" --exact --test-threads 1 > "$RUNNER_TEMP/row-linux.txt" 2>&1)
rc=$?
say "row rc=$rc $(grep -E '^test result:' "$RUNNER_TEMP/row-linux.txt" | tail -1)"
grep -A4 'panicked at' "$RUNNER_TEMP/row-linux.txt" | head -6 | cut -c1-400 | tee -a "$out"
grep -E 'linux_helper|landlock|supervisor' "$RUNNER_TEMP/row-linux.txt" | head -10 | cut -c1-400 | tee -a "$out"
for f in $(ls -1 "$debug" | sort -t- -k3 -n); do
  say "-- $f ($(wc -c < "$debug/$f") bytes)"
  head -c 3000 "$debug/$f" | tee -a "$out"
  echo | tee -a "$out"
done
sudo dmesg -T 2>/dev/null | grep -iE 'landlock|audit|denied|seccomp' | tail -20 | cut -c1-300 | tee -a "$out" || true
exit 0
