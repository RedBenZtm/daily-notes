#!/usr/bin/env bash
# Bind a GNOME shortcut to Quick Note. Idempotent: re-running updates the
# existing binding rather than adding a duplicate.
#
#   tools/install-keybinding.sh ['<Super><Shift>n'] [/path/to/daily-notes]
#   tools/install-keybinding.sh --remove
set -euo pipefail
cd "$(dirname "$0")/.."

SCHEMA=org.gnome.settings-daemon.plugins.media-keys
ITEM_SCHEMA=$SCHEMA.custom-keybinding
PREFIX=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings
NAME="Daily Notes: Quick Note"

# gsettings prints a GVariant array; parse it with python rather than tr/sed.
# (`tr -d '[]@as '` looks tempting and silently mangles every path containing
# an "a" or an "s" — which is all of them.)
paths() {
  gsettings get $SCHEMA custom-keybindings | python3 -c '
import ast, sys
raw = sys.stdin.read().strip()
if raw.startswith("@as "):
    raw = raw[4:]
for p in ast.literal_eval(raw) if raw else []:
    print(p)'
}

set_paths() {
  python3 -c '
import sys
print(str([l for l in sys.stdin.read().split() if l]).replace(chr(34), chr(39)))' \
    | xargs -0 -I{} gsettings set $SCHEMA custom-keybindings {}
}

existing=""
while read -r p; do
  [ -z "$p" ] && continue
  if [ "$(gsettings get "$ITEM_SCHEMA:$p" name 2>/dev/null)" = "'$NAME'" ]; then
    existing=$p
    break
  fi
done < <(paths)

if [ "${1:-}" = "--remove" ]; then
  [ -z "$existing" ] && { echo "no binding installed"; exit 0; }
  paths | grep -vFx "$existing" | set_paths
  gsettings reset-recursively "$ITEM_SCHEMA:$existing" 2>/dev/null || true
  echo "removed $existing"
  exit 0
fi

BINDING=${1:-'<Super><Shift>n'}
COMMAND=${2:-"$(pwd)/target/debug/daily-notes --quick-note"}

if [ -n "$existing" ]; then
  path=$existing
else
  n=0
  while paths | grep -qFx "$PREFIX/custom$n/"; do n=$((n + 1)); done
  path="$PREFIX/custom$n/"
  { paths; echo "$path"; } | set_paths
fi

gsettings set "$ITEM_SCHEMA:$path" name "$NAME"
gsettings set "$ITEM_SCHEMA:$path" command "$COMMAND"
gsettings set "$ITEM_SCHEMA:$path" binding "$BINDING"

echo "bound $BINDING -> $COMMAND"
echo "  at $path"
