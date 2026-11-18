set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=557cebe05
probes="a_second_mount_made_before_launch_keeps_the_hidden_folder_out a_second_mount_made_after_launch_keeps_the_hidden_folder_out a_mount_inside_the_project_made_before_launch_keeps_the_hidden_folder_out"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -3
if [ "$(git rev-parse --short=9 HEAD~1)" != "$tip" ]; then
  row "probe commit on $tip" yes "NOT: $(git log --format=%h --abbrev=9 -2 | tr '\n' ' ')"
  exit 1
fi
bwrap=$(command -v bwrap)
"$bwrap" --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests (the Nested leg runs)" yes yes
else
  row "this runner's bubblewrap nests (the Nested leg runs)" yes "NOT: no"
  failed=1
fi
if sudo -n true; then
  row "passwordless sudo for the bind mounts" yes yes
else
  row "passwordless sudo for the bind mounts" yes "NOT: no"
  exit 1
fi
findmnt -o TARGET,SOURCE,FSTYPE,PROPAGATION / /tmp || true
cargo test -p crazyagent --test linux_hidden_mount_probe --locked --no-run 2>&1 | tail -3
probe_out="$RUNNER_TEMP/probe.txt"
: > "$probe_out"
PROBE_OUT="$probe_out" cargo test -p crazyagent --test linux_hidden_mount_probe --locked -- --test-threads=1 > "$RUNNER_TEMP/probe-run.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/probe-run.txt" || true
for name in $probes; do
  if grep -qF -- "test $name ... ok" "$RUNNER_TEMP/probe-run.txt"; then
    row "$name" record "hidden on both legs"
  elif grep -qF -- "test $name ... FAILED" "$RUNNER_TEMP/probe-run.txt"; then
    row "$name" record "READ: $(grep -m1 -A1 "panicked at" "$RUNNER_TEMP/probe-run.txt" | grep -v 'panicked at' | head -1 | cut -c1-200) $(awk -v n="---- $name stdout ----" '$0 == n {on = 1; next} on && /^---- / {exit} on && /the secret was read/ {print; exit}' "$RUNNER_TEMP/probe-run.txt" | cut -c1-200)"
  else
    row "$name" record "NOT: not run"
    failed=1
  fi
done
echo "== what each leg saw"
cat "$probe_out"
echo "== probe run tail"
tail -40 "$RUNNER_TEMP/probe-run.txt"
exit "$failed"
