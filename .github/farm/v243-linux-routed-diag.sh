set -uo pipefail
echo "== runner"
uname -a
grep -E '^(NAME|VERSION)=' /etc/os-release
id
for f in /proc/sys/kernel/unprivileged_userns_clone /proc/sys/user/max_user_namespaces /proc/sys/user/max_net_namespaces /proc/sys/kernel/apparmor_restrict_unprivileged_userns /proc/sys/kernel/yama/ptrace_scope /sys/kernel/security/lsm; do
  echo "$f: $(cat "$f" 2>&1)"
done
ls -l /dev/net/tun 2>&1
echo "== tools"
for p in /usr/bin/slirp4netns /usr/sbin/iptables-restore /usr/sbin/iptables-save /usr/sbin/ip6tables-restore /usr/sbin/ip6tables-save /usr/bin/bwrap; do
  echo "$p -> $(readlink -f "$p" 2>&1) $(stat -L -c '%A %U %s' "$p" 2>&1)"
done
/usr/bin/slirp4netns --version 2>&1 | head -3
/usr/sbin/iptables-restore --version 2>&1
/usr/sbin/ip6tables-restore --version 2>&1
/usr/bin/bwrap --version 2>&1
echo "== build the helper"
cargo build -p crazyagent --bin crazyagent-landlock --locked 2>&1 | tail -3
helper="$PWD/target/debug/crazyagent-landlock"
test -x "$helper" || { echo "no helper at $helper"; exit 1; }
nest=(/usr/bin/bwrap --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- "$helper" --probe)
echo "== plain probe"
env -i "$helper" --probe < /dev/null
echo "plain probe rc=$?"
echo "== bare bubblewrap with its own network"
/usr/bin/bwrap --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true < /dev/null
echo "bare bwrap rc=$?"
echo "== nest probe, as nest_held asks it"
env -i "$helper" --bridge 1024 --nest -- "${nest[@]}" < /dev/null
echo "nest probe rc=$?"
echo "== route probe, as routes asks it"
start=$(date +%s%N)
env -i "$helper" --route 1 --bridge 1024 --nest -- "${nest[@]}" < /dev/null
rc=$?
echo "route probe rc=$rc in $(( ($(date +%s%N) - start) / 1000000 )) ms"
echo "== route probe under strace"
strace -f -tt -s 300 -o "$RUNNER_TEMP/route.strace" -e trace=execve,write,openat,unshare,clone,clone3,setns,socket,connect,bind,ioctl,mount,prctl,kill,wait4,exit_group env -i "$helper" --route 1 --bridge 1024 --nest -- "${nest[@]}" < /dev/null
echo "strace route probe rc=$?"
grep -cE '' "$RUNNER_TEMP/route.strace"
grep -E 'execve\(|write\(2,|exit_group|= -1 E|\+\+\+ (exited|killed)' "$RUNNER_TEMP/route.strace" | head -300
echo "== strace tail"
tail -80 "$RUNNER_TEMP/route.strace"
exit 0
