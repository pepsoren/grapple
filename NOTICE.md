# Notices

grapple is free software under the GNU General Public License, version 3 (see `LICENSE`). The workspace is
GPL-3.0-or-later; `crates/grapple-launch` is GPL-3.0-only because it contains code ported from Heroic.

## Code from other projects

- **legendary** (https://github.com/derrod/legendary), GPLv3, by the legendary contributors. Epic sign-in, the
  installed-game files and the game's launch arguments were ported from it:
  `crates/grapple-launch/src/epic_auth.rs`, `legendary.rs`, `epic.rs`.
- **Heroic Games Launcher** (https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher), GPL-3.0-only, by the
  Heroic contributors. Reading Heroic's per-game settings and building the Proton runtime from them:
  `crates/grapple-launch/src/heroic.rs`.
- **umu-launcher** (https://github.com/Open-Wine-Components/umu-launcher), GPLv3. Its runtime handling was read
  from version 1.4.0 and reimplemented: `crates/grapple-launch/src/umu.rs`.

The Epic downloader (`crates/grapple-download`) was written from a description of Epic's file formats, not from
legendary's code.

## Fonts

Figtree, by The Figtree Project Authors, under the SIL Open Font License 1.1 (shipped through the
`@fontsource/figtree` package).

## Names, logos and game art

- The grapple and tether names and logos (`brand/`, `app/src-tauri/icons/`, `app/ui/src/assets/tether/`) belong to
  their owner. The GPL covers the code, not the right to use them as your own branding.
- Game names, art and store data belong to their publishers. grapple shows them from the stores' own files and
  servers on the user's machine; none are included in this repository.
