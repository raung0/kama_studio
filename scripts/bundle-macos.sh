#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS bundling must run on macOS" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo-bundle >/dev/null 2>&1; then
  echo "error: cargo-bundle is required (cargo install cargo-bundle)" >&2
  exit 1
fi

ACTOOL=actool

ICON="$ROOT/crates/app/macos/AppIcon.icon"
OUT="$ROOT/target/macos-icon"
APP="$ROOT/target/release/bundle/osx/Kama.app"
MIN_MACOS="15.0"

if [[ ! -f "$ICON/icon.json" ]]; then
  echo "error: missing Icon Composer bundle: $ICON" >&2
  exit 1
fi

rm -rf "$OUT" "$APP"
mkdir -p "$OUT"

bundle_args=(--release --format osx --package kama)
if [[ -n "${KAMA_CARGO_FEATURES:-}" ]]; then
  bundle_args+=(--features "$KAMA_CARGO_FEATURES")
fi

MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS" \
  cargo bundle "${bundle_args[@]}"

if [[ ! -d "$APP" ]]; then
  echo "error: cargo-bundle did not produce $APP" >&2
  exit 1
fi

INFO_PLIST="$APP/Contents/Info.plist"
RESOURCES="$APP/Contents/Resources"
MACOS_DIR="$APP/Contents/MacOS"
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$INFO_PLIST")"

for tool in ffmpeg; do
  source_path="$(command -v "$tool" || true)"
  if [[ -z "$source_path" ]]; then
    echo "error: $tool is required to package rendering support" >&2
    exit 1
  fi
  cp -L "$source_path" "$MACOS_DIR/$tool"
  chmod 755 "$MACOS_DIR/$tool"
done

"$ACTOOL" "$ICON" \
  --compile "$OUT" \
  --output-format human-readable-text \
  --notices \
  --warnings \
  --output-partial-info-plist "$OUT/assetcatalog_generated_info.plist" \
  --app-icon AppIcon \
  --standalone-icon-behavior none \
  --bundle-identifier "$BUNDLE_ID" \
  --enable-on-demand-resources NO \
  --development-region en \
  --target-device mac \
  --minimum-deployment-target "$MIN_MACOS" \
  --platform macosx

if [[ ! -f "$OUT/Assets.car" ]]; then
  echo "error: actool succeeded but did not produce Assets.car" >&2
  exit 1
fi
if [[ ! -f "$OUT/assetcatalog_generated_info.plist" ]]; then
  echo "error: actool did not produce assetcatalog_generated_info.plist" >&2
  exit 1
fi

mkdir -p "$RESOURCES"
rm -rf "$RESOURCES/AppIcon.icon"
/usr/bin/ditto "$ICON" "$RESOURCES/AppIcon.icon"
cp "$OUT/Assets.car" "$RESOURCES/Assets.car"

rm -f "$RESOURCES/AppIcon.icns"

for key in CFBundleIconName CFBundleIconFile CFBundleIconFiles CFBundleIcons; do
  /usr/libexec/PlistBuddy -c "Delete :$key" "$INFO_PLIST" >/dev/null 2>&1 || true
done
/usr/libexec/PlistBuddy \
  -c "Merge $OUT/assetcatalog_generated_info.plist" \
  "$INFO_PLIST"

/usr/libexec/PlistBuddy -c 'Delete :CFBundleIconFile' "$INFO_PLIST" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :CFBundleIconFiles' "$INFO_PLIST" >/dev/null 2>&1 || true

if [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconName' "$INFO_PLIST" 2>/dev/null || true)" != "AppIcon" ]]; then
  echo "error: actool metadata did not set CFBundleIconName=AppIcon" >&2
  /usr/libexec/PlistBuddy -c 'Print' "$OUT/assetcatalog_generated_info.plist" >&2 || true
  exit 1
fi

/usr/bin/xattr -cr "$APP"
/usr/bin/codesign --force --sign - "$APP"
/usr/bin/touch "$APP"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
if [[ -x "$LSREGISTER" ]]; then
  "$LSREGISTER" -f "$APP" >/dev/null 2>&1 || true
fi

echo "$APP"
