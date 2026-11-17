#!/bin/bash
set -euo pipefail
icons="$(cd "$(dirname "$0")/../../apps/desktop/src-tauri/icons" && pwd -P)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
version="$(xcrun actool --version --output-format=human-readable-text | sed -n 's/^short-bundle-version: *//p')"
[ "${version%%.*}" -ge 26 ] 2>/dev/null || { echo "assets-car: actool '${version}' is older than 26; run this on macOS 26 with Xcode 26 or newer" >&2; exit 1; }
cp -R "$icons/AppIcon.icon" "$work/Icon.icon"
mkdir "$work/out"
xcrun actool "$work/Icon.icon" --compile "$work/out" --output-format human-readable-text --notices --warnings \
  --output-partial-info-plist "$work/out/assetcatalog_generated_info.plist" --app-icon Icon --include-all-app-icons \
  --accent-color AccentColor --enable-on-demand-resources NO --development-region en --target-device mac \
  --minimum-deployment-target 26.0 --platform macosx
test -s "$work/out/Assets.car" || { echo "assets-car: actool wrote no Assets.car" >&2; exit 1; }
info="$(assetutil --info "$work/out/Assets.car")"
grep -q '"NSAppearanceNameDarkAqua"' <<< "$info" || { echo "assets-car: the compiled Assets.car has no dark look" >&2; exit 1; }
grep -q '"Name" : "Icon"' <<< "$info" || { echo "assets-car: the compiled Assets.car names no Icon" >&2; exit 1; }
cp "$work/out/Assets.car" "$icons/Assets.car"
cd "$icons"
{
  find AppIcon.icon -type f ! -name .DS_Store -print0 | LC_ALL=C sort -z | xargs -0 shasum -a 256
  shasum -a 256 Assets.car
} > Assets.car.sha256
echo "assets-car: actool $version wrote $icons/Assets.car and Assets.car.sha256"
