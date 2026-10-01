#!/usr/bin/env bash
# Run a command with VS Code *snap* environment leakage removed.
#
# The snap build of VS Code exports GTK_PATH, GIO_MODULE_DIR, LOCPATH,
# XDG_DATA_HOME, ... pointing into /snap. Native GTK/WebKit apps started from
# its integrated terminal then load snap libraries and crash with e.g.
#   symbol lookup error: /snap/core20/.../libpthread.so.0: undefined symbol
# Usage: scripts/clean-snap-env.sh npm run tauri dev
set -euo pipefail

for v in GTK_PATH GTK_EXE_PREFIX GTK_IM_MODULE_FILE GIO_MODULE_DIR GDK_PIXBUF_MODULE_FILE \
         GDK_PIXBUF_MODULEDIR LOCPATH GSETTINGS_SCHEMA_DIR XDG_DATA_HOME; do
  unset "$v"
done
[[ -n "${XDG_DATA_DIRS_VSCODE_SNAP_ORIG:-}" ]] && export XDG_DATA_DIRS="$XDG_DATA_DIRS_VSCODE_SNAP_ORIG"
[[ -n "${XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG:-}" ]] && export XDG_CONFIG_DIRS="$XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG"
for v in $(compgen -e | grep -E '^SNAP|_VSCODE_SNAP_ORIG$' || true); do unset "$v"; done

exec "$@"
