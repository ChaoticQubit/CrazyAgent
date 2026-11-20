set -uo pipefail
kind=${1:?host kind: linux or windows}
tip=7595c0605
red=0ef406432
out="$RUNNER_TEMP/v244-win-signin-h3-$kind.txt"
failed=0
: > "$out"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"; }
git diff --quiet "$tip" -- crates Cargo.lock Cargo.toml || { row "code tip" "$tip" "NOT: crates differ"; exit 1; }
row "code tip" "$tip" "crates, Cargo.toml and Cargo.lock equal"
row "host" "$kind" "$(uname -srm)"
rows="a_windows_launch_starts_at_its_installed_boundary_with_only_the_boundary_variables a_windows_launch_with_another_home_than_its_boundary_is_refused a_full_access_launch_on_windows_is_refused_and_writes_nothing a_profile_boundary_on_windows_is_refused a_home_granted_another_container_than_its_boundary_is_refused"
module=harnesses::launch::bounded_windows_tests
ran() {
  local label=$1 file=$2
  local result reds
  result=$(grep -E '^test result:' "$file" | tail -1)
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ')
  if [ -n "$result" ] && [ -z "$reds" ] && ! grep -qE '^test result: FAILED' "$file"; then row "$label at $tip" "0 failed" "$result"; else
    row "$label at $tip" "0 failed" "NOT: $result $reds"
    grep -A4 'panicked at' "$file" | head -24 | cut -c1-300 | tee -a "$out"
    failed=1
  fi
}
if [ "$kind" = windows ]; then
  export CRAZYAGENT_HOME="$RUNNER_TEMP/crazyagent-home"
  mkdir -p "$CRAZYAGENT_HOME"
  cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/helper-$kind.txt" 2>&1 || { row "boundary helper" built NOT; tail -30 "$RUNNER_TEMP/helper-$kind.txt"; exit 1; }
  row "boundary helper" built built
  git checkout -q "$red" -- crates/crazyagent/src/harnesses/launch.rs
  cargo test -p crazyagent --lib --locked -- --test-threads 1 "$module::" > "$RUNNER_TEMP/red-$kind.txt" 2>&1
  git checkout -q "$tip" -- crates/crazyagent/src/harnesses/launch.rs
  git diff --quiet "$tip" -- crates || { row "restore after red" "$tip" NOT; exit 1; }
  if grep -qE '^test result: FAILED\. 0 passed; 5 failed' "$RUNNER_TEMP/red-$kind.txt"; then row "red rows at $red" "0 passed, 5 failed" "0 passed, 5 failed"; else
    row "red rows at $red" "0 passed, 5 failed" "NOT: $(grep -E '^test result' "$RUNNER_TEMP/red-$kind.txt" | tail -1)"
    tail -40 "$RUNNER_TEMP/red-$kind.txt" | tee -a "$out"
    failed=1
  fi
  for name in $rows; do
    grep -qE "^    $module::$name\$" "$RUNNER_TEMP/red-$kind.txt" || { row "red $name" failed "NOT in the failures"; failed=1; }
  done
  grep -E -A3 'panicked at' "$RUNNER_TEMP/red-$kind.txt" | grep -vE '^--$|RUST_BACKTRACE' | head -40 | cut -c1-260 | tee -a "$out"
  for target in --lib --tests; do
    log="$RUNNER_TEMP/clippy$target-$kind.txt"
    cargo clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then row "clippy -p crazyagent $target -D warnings" "rc 0" "rc 0"; else
      row "clippy -p crazyagent $target -D warnings" "rc 0" "NOT: rc $rc"
      grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
      failed=1
    fi
  done
  cargo test -p crazyagent --lib --locked -- --test-threads 1 "$module::" > "$RUNNER_TEMP/tip-$kind.txt" 2>&1
  ran "H3 rows" "$RUNNER_TEMP/tip-$kind.txt"
  grep -qE '^test result: ok\. 5 passed; 0 failed' "$RUNNER_TEMP/tip-$kind.txt" || { row "H3 rows count" "5 passed" NOT; failed=1; }
  for name in $rows; do
    grep -qE "^test $module::$name \.\.\. " "$RUNNER_TEMP/tip-$kind.txt" || { row "tip $name" ran "NOT"; failed=1; }
  done
  listed=$(cargo test -p crazyagent --lib --locked -- --list harnesses::launch:: 2>/dev/null | grep -cE ': test$' || true)
  cargo test -p crazyagent --lib --locked -- --test-threads 1 harnesses::launch:: > "$RUNNER_TEMP/launch-$kind.txt" 2>&1
  ran "lib harnesses::launch:: ($listed listed)" "$RUNNER_TEMP/launch-$kind.txt"
  [ "${listed:-0}" -gt 5 ] || { row "launch rows listed" "> 5" "NOT: $listed"; failed=1; }
  echo "H3 $kind failed=$failed" | tee -a "$out"
  exit "$failed"
fi
cargo check -p crazyagent --all-targets --locked --message-format short > "$RUNNER_TEMP/check-$kind.txt" 2>&1
rc=$?
issues=$(grep -cE '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" || true)
if [ "$rc" -eq 0 ] && [ "${issues:-0}" -eq 0 ]; then row "check --all-targets" "rc 0, 0 warnings" "rc 0, 0 warnings"; else
  row "check --all-targets" "rc 0, 0 warnings" "NOT: rc $rc, $issues"
  grep -E '(^|: )(error|warning)' "$RUNNER_TEMP/check-$kind.txt" | head -40 | tee -a "$out"
  failed=1
fi
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-$kind.txt" 2>&1 || { row "landlock helper" built NOT; failed=1; }
binary() {
  local label=$1 file
  shift
  file="$RUNNER_TEMP/rows-$label-$kind.txt"
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$@" > "$file" 2>&1)
  ran "$label" "$file"
}
lib=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-lib-$kind.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
if [ -n "$lib" ]; then binary "lib-launch" "$(realpath "$lib")" harnesses::launch::; else row "lib test binary" built NOT; failed=1; fi
box=$(cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-box-$kind.txt" | sed -n 's/.*Executable tests\/linux_sandbox.rs (\(.*\))$/\1/p' | head -1)
if [ -n "$box" ]; then binary "linux_sandbox" "$(realpath "$box")"; else row "linux_sandbox binary" built NOT; failed=1; fi
for name in a_linux_launch_starts_at_its_installed_boundary_with_the_agent_environment a_full_access_launch_on_linux_is_refused_and_builds_nothing a_linux_launch_with_another_home_than_its_boundary_is_refused a_full_access_pane_on_linux_is_refused_before_its_command_is_built; do
  grep -qE "^test $name \.\.\. ok\$" "$RUNNER_TEMP/rows-linux_sandbox-$kind.txt" || { row "S5a $name" ok NOT; failed=1; }
done
echo "H3 $kind failed=$failed" | tee -a "$out"
exit "$failed"
