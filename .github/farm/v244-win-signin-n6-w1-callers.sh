set -uo pipefail
list="$1"
log="$2"
shift 2
for filter in "$@"; do
  count=$(grep -cF "$filter" "$list" || true)
  echo "$filter: $count rows listed"
  [ "$count" -gt 0 ] || { echo "::error::$filter matches no row"; exit 1; }
done
listed=$(grep -cE ': test$' "$list" || true)
grep -E '^test result' "$log" || true
grep -E -A6 'panicked' "$log" || true
line=$(grep -E '^test result: ' "$log" | tail -1)
[ -n "$line" ] || { echo "::error::no test result"; tail -60 "$log"; exit 1; }
passed=$(echo "$line" | sed -E 's/.* ([0-9]+) passed.*/\1/')
failed=$(echo "$line" | sed -E 's/.* ([0-9]+) failed.*/\1/')
ignored=$(echo "$line" | sed -E 's/.* ([0-9]+) ignored.*/\1/')
[ "$failed" = 0 ] || { echo "::error::$failed caller rows failed"; exit 1; }
[ $(( passed + ignored )) = "$listed" ] || { echo "::error::passed $passed + ignored $ignored is not the $listed listed"; exit 1; }
echo "W1 caller rows: $passed passed, $ignored ignored, of $listed listed"
