set -uo pipefail
kind=${1:?host kind: linux or windows}
out="$RUNNER_TEMP/v244-linux-team-s5a-$kind.txt"
tip=3c2bf1aa3
failed=0
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then
  row "code tip" "$tip" "NOT: $code_tip"
  exit 1
fi
row "code tip" "$tip" "$code_tip"
row "host" "$kind" "$(uname -srm)"
cargo check -p crazyagent --all-targets --locked --message-format short > "$RUNNER_TEMP/check-$kind.txt" 2>&1
rc=$?
grep -E '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" | head -60 | tee -a "$out"
issues=$(grep -cE '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" || true)
if [ "$rc" -eq 0 ] && [ "${issues:-0}" -eq 0 ]; then
  row "cargo check -p crazyagent --all-targets" "rc 0, 0 warnings" "rc 0, 0 warnings"
else
  row "cargo check -p crazyagent --all-targets" "rc 0, 0 warnings" "NOT: rc $rc, $issues errors or warnings"
  tail -30 "$RUNNER_TEMP/check-$kind.txt" | tee -a "$out"
  failed=1
fi
echo "S5a $kind failed=$failed" | tee -a "$out"
exit "$failed"
