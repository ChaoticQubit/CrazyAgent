set -euo pipefail
[ "${RUNNER_ENVIRONMENT:-}" = github-hosted ] || { echo "::error::linux-packages.sh changes apt and DNS settings and runs only on a GitHub-hosted runner"; exit 1; }
packages=(build-essential pkg-config curl zstd libsqlite3-dev libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev bubblewrap libevent-dev libncurses-dev dbus gnome-keyring)
apt_options=(-o Acquire::Retries=2 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15)
hosts=(azure.archive.ubuntu.com archive.ubuntu.com security.ubuntu.com)
unresolved() {
  local host
  for host in "${hosts[@]}"; do
    getent hosts "$host" >/dev/null || printf '%s ' "$host"
  done
}
lost=$(unresolved)
if [ -n "$lost" ]; then
  link=$(ip route show default | awk '{print $5; exit}')
  [ -n "$link" ] || { echo "::error::the runner DNS does not resolve ${lost}and the runner has no default route"; exit 1; }
  echo "::warning::the runner DNS does not resolve ${lost% }; asking 1.1.1.1 and 8.8.8.8 on $link instead"
  sudo resolvectl dns "$link" 1.1.1.1 8.8.8.8
  sudo resolvectl flush-caches
  lost=$(unresolved)
  [ -z "$lost" ] || { echo "::error::public DNS does not resolve ${lost}either"; exit 1; }
fi
if ! sudo apt-get "${apt_options[@]}" update --error-on=any; then
  echo "::warning::the runner Ubuntu mirror list failed; trying archive.ubuntu.com and security.ubuntu.com directly"
  printf '%s\n' 'http://archive.ubuntu.com/ubuntu/' 'http://security.ubuntu.com/ubuntu/' | sudo tee /etc/apt/apt-mirrors.txt >/dev/null
  sudo apt-get "${apt_options[@]}" update --error-on=any
fi
sudo apt-get "${apt_options[@]}" install -y --no-install-recommends "${packages[@]}"
