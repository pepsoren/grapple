#!/usr/bin/env bash
# Install the built grapple (target/app/release/grapple) for this user, so it starts from the app launcher like
# any other app: the program in ~/.local/bin, its icons, and a desktop entry. Run after `pnpm app:build`
# (or use `pnpm app:install`, which does both). Linux only; nothing here needs root.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
src="$repo/target/app/release/grapple"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
bin="$HOME/.local/bin/grapple"

[ -x "$src" ] || { echo "No build at $src. Run: cd app/ui && pnpm app:build" >&2; exit 1; }

# The program. Copy to a temp name, then rename over the old one: a running grapple keeps its file.
mkdir -p "$(dirname "$bin")"
install -m 755 "$src" "$bin.new"
mv -f "$bin.new" "$bin"

# Icons, in the sizes the brand folder has.
for png in "$repo"/brand/png/grapple-*.png; do
  size="$(basename "$png" .png)"; size="${size#grapple-}"
  dir="$data/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$dir"
  install -m 644 "$png" "$dir/grapple.png"
done

# The launcher entry. StartupWMClass matches the window's class, so docks and window rules group it. It also opens
# grapple://invite/<id> links (game invites from tether): the desktop starts `grapple <link>` (%u), and a running
# grapple gets the link from that second start.
mkdir -p "$data/applications"
cat > "$data/applications/grapple.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=grapple
GenericName=Game launcher
Comment=Play your Steam, Epic and other games
Exec="$bin" %u
Icon=grapple
Terminal=false
Categories=Game;
Keywords=games;launcher;steam;epic;
StartupWMClass=grapple
SingleMainWindow=true
MimeType=x-scheme-handler/grapple;
EOF

# "Start at login", if it is on: point it at the installed program too.
auto="$config/autostart/grapple.desktop"
if [ -f "$auto" ]; then
  sed -i "s|^Exec=.*|Exec=\"$bin\" --hidden|" "$auto"
fi

command -v update-desktop-database >/dev/null && update-desktop-database -q "$data/applications" || true
# grapple:// links go to grapple.
command -v xdg-mime >/dev/null && xdg-mime default grapple.desktop x-scheme-handler/grapple || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$data/icons/hicolor" || true

echo "Installed grapple: $bin"
