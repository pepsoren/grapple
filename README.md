# grapple

Steam, Epic and everything else in one fast library. Press Play and the game starts, without the launchers in the
way: Steam runs hidden in the background, Epic games start with no Epic or Heroic process at all.

- One library for Steam, Epic and any program you add (Windows `.exe` files run through Proton on Linux).
- Playtime with sessions and a 14-day chart.
- A store built from Steam and Epic data that puts both prices side by side, with wishlist and sale alerts.
- Downloads and updates for Epic games done by grapple itself; Steam downloads in the same queue.
- Controller navigation, a controller test and per-game controller modes.
- Works with [tether](https://app.tethr.lol): sign in, see what friends play, invite them to a game.

Runs on Linux (any desktop) and, as a beta, on Windows. Website and downloads: https://grapple.tethr.lol

## Building

Rust (the core and the Tauri shell) and React with TypeScript (the UI, plain CSS with design tokens).

With Nix:

```bash
nix develop -c bash -c 'cd app/ui && pnpm install && pnpm app:build'
```

The app is `target/app/release/grapple`. `pnpm app:install` also installs it for the current user on Linux (program,
icons, desktop entry, `grapple://` links).

Without Nix you need Rust, Node with pnpm, and Tauri's system libraries for your platform
(https://tauri.app/start/prerequisites/).

Tests: `cargo test --workspace` and, in `app/ui`, `pnpm typecheck && pnpm check:tokens`.

To work on the UI alone, `cd app/ui && pnpm dev` serves it with a mock backend and sample data. The mock's game art
is not in this repository (it belongs to the publishers), so games show without pictures there.

The Windows build can be type-checked from Linux with `nix develop .#windows` (see `PLAN.md`); CI builds the Windows
installer and the Linux AppImage.

## Licence

GPL-3.0 (see `LICENSE`). grapple contains code ported from legendary, Heroic and umu-launcher; see `NOTICE.md`.
Game names, art and store data belong to their publishers.
