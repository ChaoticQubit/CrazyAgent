set -uo pipefail
kind=${1:?host kind: linux or windows}
tip=b7d069026
out="$RUNNER_TEMP/v244-linux-team-s5a-land-$kind.txt"
failed=0
: > "$out"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"; }
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then row "code tip" "$tip" "NOT: $code_tip"; exit 1; fi
row "code tip" "$tip" "$code_tip"
row "host" "$kind" "$(uname -srm)"
if [ "$kind" = windows ]; then
  for target in --lib --tests; do
    log="$RUNNER_TEMP/clippy$target-$kind.txt"
    cargo +"$RUST_TOOLCHAIN" clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then row "clippy -p crazyagent $target -D warnings at $tip" "rc 0" "rc 0"; else
      row "clippy -p crazyagent $target -D warnings at $tip" "rc 0" "NOT: rc $rc"
      grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
      failed=1
    fi
  done
  echo "S5a land $kind failed=$failed" | tee -a "$out"
  exit "$failed"
fi
cargo check -p crazyagent --all-targets --locked --message-format short > "$RUNNER_TEMP/check-$kind.txt" 2>&1
rc=$?
issues=$(grep -cE '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" || true)
if [ "$rc" -eq 0 ] && [ "${issues:-0}" -eq 0 ]; then row "check --all-targets at $tip" "rc 0, 0 warnings" "rc 0, 0 warnings"; else
  row "check --all-targets at $tip" "rc 0, 0 warnings" "NOT: rc $rc, $issues"
  grep -E '(^|: )(error|warning)' "$RUNNER_TEMP/check-$kind.txt" | head -40 | tee -a "$out"
  failed=1
fi
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-$kind.txt" 2>&1 || { row "landlock helper" built NOT; failed=1; }
ran() {
  local label=$1 exe=$2 file
  shift 2
  file="$RUNNER_TEMP/rows-$label-$kind.txt"
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$@" > "$file" 2>&1)
  local result reds
  result=$(grep -E '^test result:' "$file" | tail -1)
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ')
  if [ -n "$result" ] && [ -z "$reds" ]; then row "$label at $tip" "0 failed" "$result"; else
    row "$label at $tip" "0 failed" "NOT: $result $reds"
    grep -A4 'panicked at' "$file" | head -12 | cut -c1-300 | tee -a "$out"
    failed=1
  fi
}
lib=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-lib-$kind.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
if [ -n "$lib" ]; then ran "lib harnesses::launch::" "$(realpath "$lib")" harnesses::launch::; else row "lib test binary" built NOT; failed=1; fi
box=$(cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-box-$kind.txt" | sed -n 's/.*Executable tests\/linux_sandbox.rs (\(.*\))$/\1/p' | head -1)
if [ -n "$box" ]; then ran "linux_sandbox" "$(realpath "$box")"; else row "linux_sandbox binary" built NOT; failed=1; fi
echo "S5a land $kind failed=$failed" | tee -a "$out"
exit "$failed"
