#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: this script only runs on macOS" >&2
  exit 1
fi

APP="${1:-target/release/bundle/osx/Kama.app}"
if [[ ! -d "$APP" ]]; then
  echo "error: app bundle not found: $APP" >&2
  exit 1
fi
if ! command -v brew >/dev/null 2>&1; then
  echo "error: Homebrew is required" >&2
  exit 1
fi

BREW_PREFIX="$(brew --prefix)"
FRAMEWORKS="$APP/Contents/Frameworks"
INFO_PLIST="$APP/Contents/Info.plist"
EXECUTABLE_NAME="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$INFO_PLIST")"
MACOS_DIR="$APP/Contents/MacOS"
EXECUTABLE="$MACOS_DIR/$EXECUTABLE_NAME"

mkdir -p "$FRAMEWORKS"

realpath_py() {
  python3 - "$1" <<'PY'
import os
import sys
print(os.path.realpath(sys.argv[1]))
PY
}

queue=("$EXECUTABLE")
for tool in ffmpeg; do
  if [[ ! -x "$MACOS_DIR/$tool" ]]; then
    echo "error: bundled render tool missing: $MACOS_DIR/$tool" >&2
    exit 1
  fi
  queue+=("$MACOS_DIR/$tool")
done
index=0

while (( index < ${#queue[@]} )); do
  binary="${queue[$index]}"
  ((index += 1))

  while IFS= read -r dep; do
    [[ -n "$dep" ]] || continue
    case "$dep" in
      "$BREW_PREFIX"/*) ;;
      *) continue ;;
    esac

    resolved="$(realpath_py "$dep")"
    name="$(basename "$resolved")"
    bundled="$FRAMEWORKS/$name"

    newly_bundled=0
    if [[ ! -f "$bundled" ]]; then
      cp "$resolved" "$bundled"
      chmod u+w "$bundled"
      install_name_tool -id "@rpath/$name" "$bundled" 2>/dev/null || true
      newly_bundled=1
    fi

    if [[ "$(dirname "$binary")" == "$MACOS_DIR" ]]; then
      replacement="@executable_path/../Frameworks/$name"
    else
      replacement="@loader_path/$name"
    fi
    install_name_tool -change "$dep" "$replacement" "$binary"

    if (( newly_bundled )); then
      queue+=("$bundled")
    fi
  done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')
done

find "$FRAMEWORKS" -type f -name '*.dylib' -print0 | while IFS= read -r -d '' dylib; do
  /usr/bin/codesign --force --sign - "$dylib"
done
for binary in "$EXECUTABLE" "$MACOS_DIR/ffmpeg"; do
  /usr/bin/codesign --force --sign - "$binary"
done
/usr/bin/codesign --force --deep --sign - "$APP"

for binary in "$EXECUTABLE" "$MACOS_DIR/ffmpeg" "$FRAMEWORKS"/*.dylib; do
  [[ -e "$binary" ]] || continue
  if otool -L "$binary" | grep -F "$BREW_PREFIX/"; then
    echo "error: unbundled Homebrew dependency remains in $binary" >&2
    exit 1
  fi
done

echo "Bundled $(find "$FRAMEWORKS" -type f -name '*.dylib' | wc -l | tr -d ' ') Homebrew dylibs into $APP"
