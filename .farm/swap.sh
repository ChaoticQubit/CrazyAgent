set -uo pipefail
set +e
out="$RUNNER_TEMP/open-up-swap-$SIDE.txt"
: > "$out"
read -ra swaps <<< "$SWAP_ROWS"
for row in "${swaps[@]}"; do
  for run in $(seq 1 "$SWAP_RUNS"); do
    cargo +"$RUST_TOOLCHAIN" test -p crazyagent --locked --lib -- --exact "$row" > "$RUNNER_TEMP/swap-one.txt" 2>&1
    got=$(grep -E "^test $row \.\.\. " "$RUNNER_TEMP/swap-one.txt" | sed -E 's/.* \.\.\. //')
    opened=$(grep -c "was opened up" "$RUNNER_TEMP/swap-one.txt")
    why=$(grep -m1 -E "was opened up|lost its ACL|no link was planted|panicked at|called .Result::unwrap" "$RUNNER_TEMP/swap-one.txt" | cut -c1-200)
    echo "swap $row run $run: ${got:-none} opened_up_lines=$opened ${why}" | tee -a "$out"
  done
done
for row in "${swaps[@]}"; do
  ok=$(grep -cE "^swap $row run [0-9]+: ok " "$out")
  failed=$(grep -cE "^swap $row run [0-9]+: FAILED " "$out")
  opened=$(grep -E "^swap $row run [0-9]+: FAILED " "$out" | grep -cv "opened_up_lines=0 ")
  echo "SWAP $SIDE $row ok=$ok failed=$failed failed_opened_up=$opened runs=$SWAP_RUNS"
done | tee -a "$out"
