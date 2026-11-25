Build downloads for grapple, a fast game launcher (Rust workspace + Tauri 2 + React 19).

The full plan is `docs/DOWNLOADS_PLAN.md`. It is binding. Read it completely before writing code. Also read:
- `PLAN.md` (architecture, speed rules, how Steam and Epic work in grapple);
- `docs/STORE_PLAN.md`, `app/src-tauri/src/store.rs`, `app/src-tauri/src/commands.rs`, `app/src-tauri/src/main.rs`
  (the pattern for commands and events);
- `app/src-tauri/src/backend/{mod,real,mock}.rs`;
- `crates/grapple-launch/src/{epic_auth,settings,heroic,legendary,steam}.rs`;
- `crates/grapple-scan/src/{steam,epic}.rs`;
- `app/ui/src/{App.tsx,lib/ui.ts,lib/ipc.ts,lib/library.ts,lib/devbackend.ts,components/TopBar.tsx,components/SettingsMenu.tsx}`,
  `app/ui/src/styles/tokens.css`.

The approved design is `mockups/07-downloads.html`. Open it in a headless browser, or read the HTML: screens A–D.
Build exactly what it shows, with the changes the plan records.

## What to build

All of `docs/DOWNLOADS_PLAN.md`:
1. The new crate `crates/grapple-download`:
   - the Epic manifest and chunk parsers;
   - the downloader (install, update, pause, resume, cancel, verify, speed limit);
   - legendary `installed.json` and manifest writing, the queue with its own SQLite file, and the Steam watcher;
   - the opt-in Steam bridge over the Chrome DevTools Protocol;
   - the read-only `steam_probe` example.
2. The Tauri commands and `downloads-changed` event from the plan's contract, in the real and mock backends.
   `install_game` becomes an alias of `install_start`.
3. The per-game Proton choice for Epic games installed by grapple (`GameSettings.proton` plus a `LaunchSettings`
   override map). Linux only.
4. The UI:
   - the Downloads page;
   - the top-bar ring and popover;
   - the install sheet with the drive picker, replacing today's Install button behaviour;
   - the Download settings page, including "Steam download control" and its confirm;
   - "Downloads" in the avatar menu;
   - mock data in `devbackend.ts` and `backend/mock.rs`.

## Facts you can rely on

They were verified on Josh's machine on 2026-10-20 and are written into the plan:
- **Epic files:** the binary manifest layout, the chunk layout, the chunk URL format, and that chunks download with no auth header.
- **Josh's files:** the real `installed.json` entry and his real Steam `content_log.txt` and appmanifest lines.
- **Steam bridge:** the `SteamClient` function names, from the `@steambrew/client` 5.5.0 typings.
- **The one gap:** the exact no-dialog Steam install sequence. Nobody could test it in the cloud, so make it easy to change.

## Hard rules

- **Cross-platform.** grapple targets Windows and every Linux desktop. Nothing may depend on Hyprland, NVIDIA or
  systemd. Linux-only code needs a `cfg` and a fallback.
- **No real accounts, ever.**
  - Never call real Epic, Steam, tether or IsThereAnyDeal endpoints, and never start Steam.
  - Never read or create a real `user.json`, Steam files or tokens.
  - Never sign in, buy anything, or change any account.
  - All tests use temp dirs, generated fixtures and local fake servers (axum, as in `crates/grapple-tether` and
    `crates/grapple-store` tests).
  - The only network you use is package registries (crates.io, npm).
- **Secrets stay out of logs.** Never log or print tokens, exchange codes, cookies or the bridge websocket URL. Tokens only come
  from `EpicAuth::session()`.
- **Steam bridge safety.**
  - Connect only to loopback.
  - Accept only a `SharedJSContext` target on `steamloopback.host`.
  - Evaluate only fixed expression templates with validated integer arguments, never interpolated strings.
  - The bridge is off by default.
- **No legendary code.** Do not copy legendary's code or comments (it is GPL-3; the license decision is open).
  Implement from the plan's format description.
- **Stay in scope.**
  - Don't change `crates/grapple-core`'s schema; downloads use their own `downloads.db`.
  - Don't edit `mockups/`.
  - Don't reformat files you don't otherwise change.
  - No new UI beyond mockup 07 and the plan.
- **Styling.**
  - Every colour, radius and shadow is a token from `tokens.css`; `pnpm check:tokens` must pass.
  - Terse copy as in the mockup.
  - Green only for progress.
  - Official art only.
- **Speed.** Rust does all work. The UI only renders state pushed by `downloads-changed`, at most 4 per second, with no
  UI polling. Commands that touch disk or network are async.

## Setup

The cloud machine has no Tauri system libraries. Install them first (Ubuntu/Debian):
```bash
sudo apt-get update && sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libdbus-1-dev pkg-config \
  build-essential curl
```
Then:
- Rust: stable, edition 2024. The repo builds with rustc 1.99.
- Node 26 and pnpm 12: `cd app/ui && pnpm install`.
- If a system package isn't available, note it in the PR and still run all crate tests.

## How to work

- Work on a new branch `downloads` from `master`.
- Commit in logical steps with clear messages, and push the branch.
- When done, open a pull request to `master`. Never merge it, and never push to `master`.
- The PR description must contain:
  - what you built;
  - how you tested it, with real test counts;
  - every deviation from the plan and why;
  - the "Open" list from the plan, updated;
  - the full "For Josh to test on his machine" checklist from the plan, with exact commands. That includes how
    to build and run the app: `cd app/ui && pnpm app:build`, then `target/app/release/grapple`, and
    `cargo run -p grapple-download --example steam_probe`.
- Before opening the PR, run:
  ```bash
  nice -n 19 cargo test -p grapple-download -p grapple-scan -p grapple-launch
  ```
  ```bash
  cargo build -p grapple
  ```
  ```bash
  cargo build -p grapple --no-default-features --features mock
  ```
  ```bash
  cd app/ui && pnpm typecheck && pnpm check:tokens && pnpm build
  ```
- If you can run a headless browser, start the UI with `pnpm dev` (port 5180, browser mock backend). Screenshot
  the four screens next to the mockup's screens and attach the comparison to the PR.
- If you find a real conflict between the plan and the code, choose the option that keeps grapple safe and
  cross-platform, write down why in the PR, and continue. Don't stop to ask.
