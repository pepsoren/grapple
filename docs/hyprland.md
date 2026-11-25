# grapple on Hyprland / Omarchy

grapple never edits your Hyprland config. These are the rules it is designed around; put them in
`~/.config/hypr/` yourself (Omarchy's Lua config, `o.window` is Omarchy's helper around `hl.window_rule`).
Window class is `grapple`, title `grapple`. A plain `grapple` (or the tray's "Show grapple") shows the window of
the running instance; it never starts a second process. Finding a game from the keyboard is the in-window
palette: type any letter on Home, Enter plays, Tab opens details.

## Rules for the main window and the playing card

While a game runs, the main window shrinks to the 380x92 "playing" card (and returns to its previous size when
the game exits). Tauri asks the compositor for that size; a **tiled** window ignores size requests, so the card
would fill its tile. Make the main window float so the resize works:

```lua
o.window({ class = "grapple", title = "^grapple$" }, { float = true })
o.window({ class = "grapple", title = "^grapple$" }, { center = true })
o.window({ class = "grapple", title = "^grapple$" }, { size = { 1280, 800 } })
o.window({ class = "grapple", title = "^grapple$" }, { tag = "-default-opacity", opacity = "1 1" })
```

If you prefer grapple tiled, skip those rules: everything works, the playing card just occupies the tile.
The playing card has no window buttons: the Stop button ends the game, Esc (on the card) gives the full window
back while the game keeps running.

## Rules for the store window (Buy)

Buy opens Steam's or Epic's own checkout in a separate window. Its class is `grapple-store` (its own Wayland
app id, so the main window's rules never match it) and its title is `Steam · <page>` or `Epic Games · <page>`.
Payment popups it opens (PayPal, a card's 3-D Secure check) have the same class and the title
`Steam · <host> · <page>`. Float them so they sit over grapple at the size they ask for (1100x820, popups 600x760):

```lua
o.window({ class = "grapple-store" }, { float = true })
o.window({ class = "grapple-store" }, { center = true })
o.window({ class = "grapple-store" }, { tag = "-default-opacity", opacity = "1 1" })
```

The window has no address bar and no grapple controls (it has no connection to grapple at all). Keys:
Alt+Left / Alt+Right back and forward, F5 or Ctrl+R reload, Ctrl+W (or Super+Q) closes it. Its cookies live in
`~/.local/share/grapple/store-web`; delete that folder to sign the store window out of everything.

## Notes

- Closing the window (Super+Q / killactive) hides it; the process keeps running in the tray. Quit from the
  tray menu or the avatar menu.
- Start at login is an opt-in setting (avatar menu): it writes
  `~/.config/autostart/grapple.desktop` (`grapple --hidden`). With Omarchy's uwsm session you can instead add
  `o.launch_on_start("grapple --hidden")` to `~/.config/hypr/autostart.lua`.
- The window is opaque (its own background is the theme's canvas colour), so a part of the page WebKit has not
  painted yet never shows the desktop behind it.
- Rendering: WebKitGTK draws the page on the GPU (its DMA-BUF renderer). On NVIDIA + Wayland that renderer used to
  end the window with `Error 71 (Protocol error)` when a second surface appeared; grapple sets
  `__NV_DISABLE_EXPLICIT_SYNC=1` for its own process (unless the variable is already set), which keeps the GPU
  path and the window alive. `WEBKIT_DISABLE_DMABUF_RENDERER=1` in the environment still forces the old CPU
  path (slow: about 28 ms of CPU per frame when scrolling Home, against 3 ms on the GPU).
- Nothing grapple sets for its window reaches the games: Steam, Proton/umu and manual games get the session
  environment grapple was started with, each in its own systemd user scope (`app-grapple-*.scope`), and Steam is
  started outside grapple's process tree (its parent is the user manager, as when you start it from the desktop).
- A GTK text-scaling factor below 1 (Omarchy on this machine uses 0.909) makes the webview's pixel ratio
  0.909, so the UI is drawn about 10% smaller than the mockups' CSS pixels for the same window size.
