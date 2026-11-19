set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out() {
  printf '%s\n' "$*"
  printf '%s\n\n' "$*" >> "$summary"
}
bwrap=$(command -v bwrap) || {
  out "P0 no bwrap"
  exit 1
}
os=$(. /etc/os-release && echo "$PRETTY_NAME")
out "P0 $os, kernel $(uname -r), $("$bwrap" --version), $(unshare --version | head -1)"
out "P6 apparmor_restrict_unprivileged_userns=$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo absent) max_user_namespaces=$(cat /proc/sys/user/max_user_namespaces)"
out "P6 aa-status bwrap: $(sudo aa-status 2>/dev/null | grep -i bwrap | tr -s ' \n' ' ')"
out "P6 /etc/apparmor.d bwrap files: $(ls /etc/apparmor.d 2>/dev/null | grep -i bwrap | tr '\n' ' ')"
nest=(--die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc)
if "$bwrap" "${nest[@]}" --unshare-net -- /bin/true 2> "$RUNNER_TEMP/nest.err"; then
  out "P6 bwrap nests: yes"
else
  out "P6 bwrap nests: NO $(tr '\n' ' ' < "$RUNNER_TEMP/nest.err")"
fi
cut=(unshare -r -m --propagation private)
if unshare --help 2>&1 | grep -q -- --map-current-user; then
  mapped=(unshare --map-current-user -m --propagation private)
else
  mapped=()
fi
counted='grep -c "master:\|shared:" /proc/self/mountinfo; echo "uid $(id -u) caps $(grep CapEff /proc/self/status)"'
told() {
  local label=$1
  shift
  timeout 60 "$@" > "$RUNNER_TEMP/told.log" 2>&1
  local rc=$?
  out "$label: $(tr '\n' ' ' < "$RUNNER_TEMP/told.log") rc=$rc"
}
told "P6 Nested, root-mapped cut" "$bwrap" "${nest[@]}" -- "${cut[@]}" sh -c "$counted"
if [ "${#mapped[@]}" -gt 0 ]; then
  told "P6 Nested, current-user cut" "$bwrap" "${nest[@]}" -- "${mapped[@]}" sh -c "$counted"
else
  out "P6 Nested, current-user cut: this unshare has no --map-current-user"
fi
told "P6 Nested, no cut" "$bwrap" "${nest[@]}" -- sh -c "$counted"
case=$(mktemp -d)
mkdir -p "$case/src/H" "$case/alias" "$case/held"
echo "secret words" > "$case/src/H/secret.txt"
closed=(--die-with-parent --unshare-ipc --unshare-pid --unshare-uts --proc /proc --dev /dev --tmpfs /tmp)
for root in /usr /bin /sbin /lib /lib32 /lib64 /libx32 /etc; do
  closed+=(--ro-bind-try "$root" "$root")
done
closed+=(--bind "$case" "$case")
pid=0
started() {
  local log=$1
  shift
  rm -f "$case/go"
  timeout 120 "$@" > "$log" 2>&1 &
  pid=$!
  for _ in $(seq 1 300); do
    grep -q '^ready$' "$log" 2>/dev/null && return 0
    sleep 0.1
  done
  return 1
}
reader="echo ready; while [ ! -e '$case/go' ]; do sleep 0.1; done"
p3() {
  local label=$1
  shift
  local log="$RUNNER_TEMP/p3.log"
  started "$log" "$@" sh -c "$reader; cat '$case/alias/H/secret.txt' 2>&1; echo end"
  sudo -n mount --bind "$case/src" "$case/alias"
  touch "$case/go"
  wait "$pid"
  local rc=$?
  sudo -n umount -l "$case/alias"
  out "P3 $label: $(tr '\n' ' ' < "$log") rc=$rc"
}
p3 "Nested, no cut (base)" "$bwrap" "${nest[@]}" --
p3 "Nested, cut" "$bwrap" "${nest[@]}" -- "${cut[@]}"
p3 "Bubblewrap, no cut (base)" "$bwrap" "${closed[@]}" --
p3 "Bubblewrap, cut" "$bwrap" "${closed[@]}" -- "${cut[@]}"
p4() {
  local label=$1
  shift
  local log="$RUNNER_TEMP/p4.log"
  local image="$RUNNER_TEMP/held.img"
  rm -f "$image"
  truncate -s 32M "$image"
  mkfs.ext4 -q -F "$image"
  mkdir -p "$case/held"
  local device
  device=$(sudo -n losetup -f --show "$image")
  sudo -n mount "$device" "$case/held"
  echo "held words" | sudo -n tee "$case/held/file.txt" > /dev/null
  started "$log" "$@" sh -c "$reader; cat '$case/held/file.txt' 2>&1; grep -c ' $case/held ' /proc/self/mountinfo; echo end"
  local unmounted detached removed
  unmounted=$(sudo -n umount "$case/held" 2>&1; echo "rc=$?")
  detached=$(sudo -n losetup -d "$device" 2>&1; echo "rc=$?")
  removed=$(rmdir "$case/held" 2>&1; echo "rc=$?")
  local during
  during=$(sudo -n losetup -l -n -O NAME,BACK-FILE 2>/dev/null | grep -F "$image" | tr '\n' ' ')
  touch "$case/go"
  wait "$pid"
  local after
  after=$(sudo -n losetup -l -n -O NAME,BACK-FILE 2>/dev/null | grep -F "$image" | tr '\n' ' ')
  out "P4 $label: host umount [$unmounted] losetup -d [$detached] rmdir [$removed] loop while running [$during] agent [$(tr '\n' ' ' < "$log")] loop after the agent ended [$after]"
  sudo -n losetup -d "$device" 2> /dev/null
}
p4 "Nested, no cut (base)" "$bwrap" "${nest[@]}" --
p4 "Nested, cut" "$bwrap" "${nest[@]}" -- "${cut[@]}"
p5() {
  local label=$1
  shift
  local where=/srv/v24x-automount
  sudo -n mkdir -p "$where"
  local made
  made=$(sudo -n systemd-mount --automount=yes --collect -t tmpfs tmpfs "$where" 2>&1; echo "rc=$?")
  local kind
  kind=$(findmnt -n -o FSTYPE "$where" 2>&1 | tr '\n' ' ')
  local log="$RUNNER_TEMP/p5.log"
  started "$log" "$@" sh -c "$reader; ls -A '$where' 2>&1; echo \"ls rc=\$?\"; stat -f -c %T '$where' 2>&1; echo end"
  touch "$case/go"
  wait "$pid"
  out "P5 $label: systemd-mount [$made] host type before [$kind] agent [$(tr '\n' ' ' < "$log")]"
  sudo -n systemd-mount --umount "$where" > /dev/null 2>&1
}
p5 "Nested, no cut (base)" "$bwrap" "${nest[@]}" --
p5 "Nested, cut" "$bwrap" "${nest[@]}" -- "${cut[@]}"
sudo -n umount -l "$case/alias" 2> /dev/null
exit 0
