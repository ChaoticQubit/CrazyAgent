set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
/usr/bin/slirp4netns --version 2>&1 | head -1
echo "== build at the red tip"
cargo test -p crazyagent --lib --test linux_sandbox --locked --no-run 2>&1 | tail -3
helper="$PWD/target/debug/crazyagent-landlock"
test -x "$helper" || { row build "the helper builds" "no helper at $helper"; exit 1; }
rules=(--read /usr --read /lib --read /lib64 --read /bin --read /etc)
echo "== arms A and B: a sibling with its own user and network namespaces"
unshare --user --map-current-user --net sleep 600 &
sib=$!
for _ in $(seq 50); do
  [ "$(readlink /proc/$sib/ns/net 2>/dev/null)" != "$(readlink /proc/self/ns/net)" ] && break
  sleep 0.1
done
echo "sibling $sib net $(readlink /proc/$sib/ns/net) user $(readlink /proc/$sib/ns/user)"
nsenter --user=/proc/$sib/ns/user --net=/proc/$sib/ns/net --preserve-credentials /bin/true
a0=$?
row "arm A control: unconfined nsenter by /proc/<sibling>/ns" "exit 0" "exit $a0"
"$helper" "${rules[@]}" --read /proc -- /usr/bin/nsenter --user=/proc/$sib/ns/user --net=/proc/$sib/ns/net --preserve-credentials /bin/true
a1=$?
row "arm A: Landlock-confined nsenter by /proc/<sibling>/ns, /proc readable" "refused if Landlock's ptrace rule applies" "exit $a1"
exec 5</proc/$sib/ns/user 6</proc/$sib/ns/net
"$helper" "${rules[@]}" --read /proc -- /bin/ls -l /proc/self/fd
"$helper" "${rules[@]}" -- /usr/bin/nsenter --user=/proc/self/fd/5 --net=/proc/self/fd/6 --preserve-credentials /bin/true
b1=$?
row "arm B: Landlock-confined nsenter by inherited descriptors, no /proc rule" "exit 0 if the hop plan holds" "exit $b1"
exec 5<&- 6<&-
kill "$sib" 2>/dev/null
echo "== red unit row at the red tip"
unit=sandbox::linux::egress::tests::slirp_reads_and_writes_only_the_files_it_needs
cargo test -p crazyagent --lib --locked -- "$unit" --exact > "$RUNNER_TEMP/red-unit.txt" 2>&1
cat "$RUNNER_TEMP/red-unit.txt" | tail -30
at=$(grep -m1 -oE 'panicked at [^:]+:[0-9]+' "$RUNNER_TEMP/red-unit.txt" | sed 's/panicked at //')
if grep -qF -- "test $unit ... FAILED" "$RUNNER_TEMP/red-unit.txt" && grep -q 'slirp read outside its files' "$RUNNER_TEMP/red-unit.txt" && [[ "$at" == */egress.rs:* ]]; then
  row "red slirp_reads_and_writes_only_the_files_it_needs at 271563657" "fails at its read assertion" "fails at $at"
else
  row "red slirp_reads_and_writes_only_the_files_it_needs at 271563657" "fails at its read assertion" "NOT: panic at ${at:-none}"
  failed=1
fi
echo "== linux_sandbox at the red tip"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/red-file.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/red-file.txt"
grep -E 'panicked at' "$RUNNER_TEMP/red-file.txt" | sed -E "s/^thread '([^']+)'.*panicked at /\1 at /" | sort | uniq
row "linux_sandbox at 271563657" "routed :2750 reds, P6 :3156, P5 :2299 and the N8 baseline" "$(grep -E '^test result' "$RUNNER_TEMP/red-file.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/red-file.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | tr '\n' ' ')"
echo "== arm C: the flag removed on this runner only"
sed -i '/^            "--enable-sandbox",$/d' crates/crazyagent/src/sandbox/linux/egress.rs
changed=$(git diff --numstat | tr '\t' ' ')
echo "diff: $changed"
if [ "$changed" != "0 1 crates/crazyagent/src/sandbox/linux/egress.rs" ]; then
  row "arm C patch" "one line removed from egress.rs" "NOT: $changed"
  exit 1
fi
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -2
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
echo "test binary $bin"
for r in a_routed_agent_looks_names_up_through_the_sandbox_resolver a_routed_agent_reaches_its_any_host_port_and_not_a_bridged_port_on_another_host; do
  trace="$RUNNER_TEMP/$r.strace"
  strace -f -o "$trace" -e trace=execve,clone,clone3,fork,vfork,openat,open,ioctl,connect,setns "$bin" --exact "$r" > "$RUNNER_TEMP/$r.txt" 2>&1
  echo "== arm C $r"
  grep -E '^test |panicked at|crazyagent-landlock:|slirp' "$RUNNER_TEMP/$r.txt" | head -20
  awk 'NR == FNR {
         if ($0 ~ /execve\("\/usr\/bin\/slirp4netns"/) s[$1] = 1
         if (($1 in s) && $0 ~ /(clone|clone3|fork|vfork)/ && match($0, /= [0-9]+$/)) s[substr($0, RSTART + 2)] = 1
         next
       }
       ($1 in s) && $0 ~ /(openat|open|ioctl|connect|setns|execve)\(/ { print }' "$trace" "$trace" | cut -c1-220 | head -120
  echo "== arm C $r files slirp opened"
  awk 'NR == FNR {
         if ($0 ~ /execve\("\/usr\/bin\/slirp4netns"/) s[$1] = 1
         if (($1 in s) && $0 ~ /(clone|clone3|fork|vfork)/ && match($0, /= [0-9]+$/)) s[substr($0, RSTART + 2)] = 1
         next
       }
       ($1 in s) && match($0, /open(at)?\([^"]*"[^"]+"/) { p = substr($0, RSTART, RLENGTH); sub(/^[^"]*"/, "", p); sub(/"$/, "", p); print p }' "$trace" "$trace" | sort | uniq -c
  row "arm C $r, flag removed" "passes if the flag was the only fault" "$(grep -E "^test $r \.\.\. " "$RUNNER_TEMP/$r.txt" | sed -E 's/^.* \.\.\. //'); $(grep -m1 -oE 'panicked at [^:]+:[0-9]+' "$RUNNER_TEMP/$r.txt")"
done
echo "== arm C every routed row, flag removed, no strace"
"$bin" routed > "$RUNNER_TEMP/routed.txt" 2>&1
grep -E '^test |panicked at|^test result' "$RUNNER_TEMP/routed.txt"
row "arm C routed rows, flag removed" "pass if the flag was the only fault" "$(grep -E '^test result' "$RUNNER_TEMP/routed.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/routed.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | tr '\n' ' ')"
git checkout -- crates/crazyagent/src/sandbox/linux/egress.rs
exit "$failed"
