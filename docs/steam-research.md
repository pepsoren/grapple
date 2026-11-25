# Steam: can Grapple avoid running the full Steam client?

Researched 2026-10-18. Scope: legitimate options only. No emulators (Goldberg etc.), no `steam_api` replacements,
no Steamless/patching. VAC games (CS2) are out of scope for anything that touches the client.

## Verdict

1. **There is no supported, lighter client that games can connect to.** The "verification" Josh wants to run on
   its own is not a separate function. It is `steamclient.so`, and it lives inside the core `steam` process
   together with the Steam UI code (`steamui.so`, `vgui2_s.so`). Games talk to that process over local IPC.
2. **SteamCMD cannot stand in for it.** It has `app_run`, but for games that use Steamworks/Steam DRM it does not
   work as the client: games report "Steam not found" or just start the full GUI client. Valve documents
   SteamCMD as a tool for installing/updating dedicated servers. No Valve statement or report shows otherwise.
3. **`-no-browser` (turn off CEF/steamwebhelper) is dead.** Community reports say it stopped working by
   Feb 2023, and the current client binary on this machine (build 1791918634, Oct 2026) no longer contains the
   string. Steam restarts the web helper if you kill it. Nothing in the current client turns it off.
4. **What is actually heavy is the web helper, not the verification.** Measured now on this machine: `steam`
   core about 220 MiB (PSS) versus 9 `steamwebhelper` processes about 1.1 GiB (PSS) with the Library window open.
   That is the cost of Steam being up at all. We cannot remove it, but we can avoid paying it all day.
5. **Recommendation:** keep the plan's `LaunchStrategy::Steam`. Default to "start Steam on demand, silent, and shut
   it down when the last Steam game exits" (cold start measured at about 25-60 s from your own logs, partly
   Proton setup). Offer "Keep Steam ready" as an opt-in that costs about 1.3 GiB RAM. Offline mode does not make
   Steam lighter. Details and the measurement plan are at the bottom.

## 1. How a game's steam_api reaches the Steam client

Findings:

- Valve's Steamworks docs state that a running Steam client is required: "A running Steam client is required to
  provide implementations of the various Steamworks interfaces", and that the game must be owned by the active
  account. They also say all Steam API calls are "transparently marshaled and sent via an RPC/IPC mechanism
  (A cross-process pipe)". Source: https://partner.steamgames.com/doc/sdk/api (fetched 2026-10-18).
- `SteamAPI_RestartAppIfNecessary` is a game-side call that relaunches the game through Steam when it was started
  outside it (same page). This is why a DRM game started by hand just opens Steam.
- Observed on this machine (Steam running, pid 250989, read-only inspection):
  - `/proc/250989/maps` shows the `steam` process itself has `steamclient.so`, `steamui.so` and `vgui2_s.so`
    mapped. This is the process that does the login, the license check and the IPC server for games.
  - No `steamwebhelper` process maps `steamclient.so`. The web helper only renders the UI (Library, store, chat,
    overlay pages).
  - IPC objects: `~/.steam/steam.pid` (contains 250989), the named pipe `~/.steam/steam.pipe`,
    `/dev/shm/u1000-ValveIPCSharedObj-Steam`, and `/tmp/steam_chrome_shmem_uid1000_spid250989` (a Unix socket
    that the web helper listens on for the `steam` process).
  - `~/.steam/sdk64` points to `~/.local/share/Steam/linux64`, which holds the 64-bit `steamclient.so`. A game's
    `libsteam_api.so` loads this stub library and connects to the running `steam` process through those IPC objects.
    (The exact handshake is not publicly documented; the file list above is what I could observe. Not
    reverse-engineered further on purpose.)
- So the process that must be alive is `steam` (the core). The web helper is not part of the license check,
  but the core's UI code is wired to it, and the client starts it unconditionally (see 3).

Not confirmed: whether the core `steam` would keep serving game IPC if the web helper were absent. Nobody
documents a supported way to test this (see 3).

## 2. SteamCMD as the client

- `app_run <appid>` exists. A Steam forum thread confirms "app_run 440" launches Team Fortress 2
  (Aug 15, 2023): https://steamcommunity.com/discussions/forum/0/3492006259503374390/
- It is not a client that games can use:
  - Thread "Games launched using SteamCMD cannot find Steam or always launch the full (GUI) client" (Apr 21, 2025):
    Celeste says "Steam not found!", The Escapists 2 launches the normal GUI Steam. Accepted answer: "SteamCMD is not
    the client, it's just a console with bare minimum features", games that use Steam's DRM need the real client.
    https://steamcommunity.com/discussions/forum/1/601902244366984610
  - Thread asking Valve to add Steam DRM support to `app_run` to save about 1 GB RAM (Jun 13, 2023). Replies quote
    the Valve wiki line that SteamCMD's "primary use is to install and update various dedicated servers" and
    nothing was added. The poster later claimed a way to disable steamwebhelper and still use the library, with
    no details and no replies (unverifiable).
    https://steamcommunity.com/discussions/forum/0/3817410974683600863
  - Nov 2018: "if the game is hooked into steam's DRM scheme, said game will attempt to launch the steam client
    before running." https://steamcommunity.com/app/221410/discussions/0/2798319091587320581/
  - Linux specifically: `app_run` failed with "launch failed (Missing executable)" for a native game that worked
    on Windows (Jul 29, 2023), unresolved in the thread.
    https://steamcommunity.com/app/221410/discussions/0/3806155895401090593/
- SteamCMD is fine for the dedicated-server API (`SteamGameServer`), which does not need a client. That is not
  what a normal game uses.
- Valve's wiki page for SteamCMD (https://developer.valvesoftware.com/wiki/SteamCMD) could not be fetched
  (403 / bot check, I did not try to get around it). The statements above about the wiki are relayed from the
  forum threads, so treat the exact wording as second-hand.

Verdict for Q2: not a viable client for Steamworks/DRM games, as of the 2023-2025 reports. I did not run SteamCMD
(research only, and it is not installed). I found no report of the opposite.

## 3. Running the normal client with no UI/web helper

- `-no-browser`: the Valve wiki (via search snippet, Valve page itself blocked) and the Debian man page text say
  it "unconditionally disables CEF" and falls back to the old friends/chat UI. Forum thread started Feb 2019,
  "-no-browser is removed, no longer supported", worked until about Mar 2022, confirmed broken by Feb 2023, and
  killing the web helper makes it restart: https://steamcommunity.com/discussions/forum/1/1798529872635342264
  Local evidence: `strings ubuntu12_32/steam` (build 1791918634, file dated 2026-10-13) has no `no-browser` or
  similar string. I did not run Steam with the flag (not allowed to start/stop it). Conclusion: do not rely on it.
- Flags actually present in this client binary (from `strings`): `-silent`, `-nofriendsui`, `-friendsui`,
  `-gamepadui`, `-steamos`, `-steamos3`, `-steamdeck`, `-console`, `-noverifyfiles`, `-skipinitialbootstrap`,
  `-nobootstrapperupdate`, `-ifrunning`, `-exitsteam`, `-singleapp`, `-cef-disable-gpu`, `-cef-force-gpu`,
  `-cef-disable-sandbox`, `-cef-disable-gpu-sandbox`, `-no-cef-sandbox`, `-textclient` (purpose unknown, not
  documented anywhere I found), `-login`, `-download`. `-nochatui` and `-tenfoot` do not appear in the strings
  (tenfoot may be parsed differently; unconfirmed).
- `-silent` only starts Steam without showing the main window. Josh's logs show Grapple-style launches already
  worked this way: `steam -silent steam://rungameid/730`. I believe the web helper and its renderers still start
  (the UI is rendered by CEF even when hidden), but I could not measure a `-silent` instance because the running
  Steam was started without it and I must not restart it. **Unconfirmed, measure it** (plan below).
- `-gamepadui` / `-steamos3` (Big Picture / Steam Deck UI) still use CEF/steamwebhelper. Not lighter in any
  source I found. A May 2025 client update says the "steamwebhelper process's memory usage has been reduced when
  launching games or switching to and from Big Picture Mode" (Steam Deck beta 5/16/25, linuxiac, 2025-05-20):
  https://linuxiac.com/steam-client-update-improves-linux-game-recording/
  So Valve already trims the web helper while a game runs; the saving is not quantified.
- `-cef-disable-gpu` and friends only change GPU use, they do not remove processes. Not shown to reduce RAM.
- Settings people use to cut web helper memory (not flags): Library "low performance mode", "low bandwidth mode",
  turn off "GPU accelerated rendering in web views", start on Library. Reported effects "slow the leak down"
  (Nov 2023, https://steamcommunity.com/groups/SteamClientBeta/discussions/3/4027969376952109675) and 400 MB
  at start growing to about 2 GB over time (Mar 2024 to Nov 2025,
  https://steamcommunity.com/discussions/forum/1/4298194004199893826). Level1Techs (Mar 2024) reports the same
  creep: https://forum.level1techs.com/t/steam-client-webhelper-memory-usage/207918
  I could not find where these are stored in `localconfig.vdf`, so Grapple should not try to write them.
- "Headless" Steam setups (e.g. Steam Headless docker project) run the full client in a virtual desktop; I found
  no headless mode that skips CEF. https://github.com/Steam-Headless/docker-steam-headless

Measured right now (Steam up since 16:45 today, started without `-silent`, Library window open, no game
running, 15.9 GiB RAM total, about 10.5 GiB used; `ps` RSS, `/proc/<pid>/smaps_rollup` PSS):

| Process | RSS | PSS |
|---|---|---|
| `steam` (core, pid 250989) | 226 MiB | 220 MiB |
| `steamwebhelper` main (pid 251197) | 371 MiB | 252 MiB |
| `steamwebhelper` renderer (pid 251265, likely the Library UI) | 651 MiB | 563 MiB |
| other 7 `steamwebhelper` (GPU/network/zygotes/crashpad) | about 830 MiB | about 310 MiB |
| **All 9 webhelper processes** | **1.81 GiB** | **about 1.10 GiB** |
| Whole Steam | about 2.0 GiB | **about 1.3 GiB** |

RSS double-counts shared pages, PSS is the fair number. Reading: the web helper is about 5x the core and is
the part that cannot be avoided.

## 4. Offline mode

- Steam's offline mode lets Steam start with no network if you logged in with "remember me" and the games are
  fully updated; DRM and online games may still refuse (Steam help; the support page could not be fetched
  here, so this comes from search summaries and the Steam forum):
  https://steamcommunity.com/discussions/forum/0/1754652960103645734 and https://help.steampowered.com/en/faqs/view/0E18-319B-E34B-B2C8
- It does not remove the client or the web helper, so it gives no RAM/CPU benefit. It only helps when the network
  is down. It may save the login wait on a slow connection (not measured). Force-closing Steam can lose the stored
  data needed for offline mode, so Grapple should use `steam -shutdown`, not kill.
- Not tested here (cannot log in or toggle settings).

## 5. Making "hidden Steam" cheaper or faster

- **Cold start cost, from Josh's own logs** (`~/.local/share/Steam/logs/console-linux.txt` vs
  `gameprocess_log.txt`), time from `Startup - Steam Client launched with ... -silent steam://rungameid/<id>` to
  the game process being tracked. These include Proton prefix work, so Steam's own share is smaller:
  - 2026-10-03 21:37:52 to 21:38:32: 40 s (no `-silent`, app 1144200)
  - 2026-10-04 21:46:20 to 21:46:48: 28 s (no `-silent`, app 1144200)
  - 2026-10-10 14:48:02 to 14:48:37: 35 s (`-silent`, app 1144200)
  - 2026-10-11 01:17:27 to 01:17:51: 24 s (`-silent`, app 730)
  - 2026-10-13 20:55:05 to 20:56:05: 60 s (`-silent`, app 1144200)
  So a cold launch is roughly 25-60 s today. A warm launch was not logged; measure it.
- **`-applaunch <id>` vs `steam://rungameid/<id>`:** both end up in the same running client; I found no source
  comparing latency. Josh's logs show `steam://rungameid/<id>` working with `-silent` on a cold start. The Debian
  man page documents `-applaunch APP_ID [APP_ARG...]` for passing arguments; community reports say extra
  arguments in the `steam://` URL can be ignored (search summary, not verified). Pick one and measure both.
  Both start through `steam.sh` (bash, runtime checks) before reaching the running client, so there is a fixed
  shell cost even when Steam is warm; unmeasured.
- **`-silent` plus `-nofriendsui`:** avoids the main window and friends window popping up on launch (Valve wiki via
  search snippet). Does not remove processes (unconfirmed, see 3).
- **`-noverifyfiles` / `-skipinitialbootstrap` / `-nobootstrapperupdate`:** exist in the binary. They skip file
  verification and update checks, which could shorten cold start, but they skip work Valve wants done (updates).
  I would not use them by default; treat as something to test for latency only if cold start dominates.
- **Keep warm:** the only measured trade is memory: about 1.3 GiB resident (of 15.9 GiB) versus 25-60 s cold start.
  The web helper also grows over time, so a long-lived hidden Steam is the worst case. Restarting Steam when idle
  and no game is running resets it.
- **Shutdown:** `steam -shutdown` (Valve wiki, man page) is the clean way; do not kill processes.
- Valve already lowers web helper memory while a game runs (May 2025 release notes above), so "Close Steam when
  I quit" mostly matters for the time Steam stays up after the game.

## Concrete recommendation for the Steam `LaunchStrategy`

Do:
1. Keep the trait boundary. The current only implementation is `SteamClientStrategy`. Nothing lighter exists; do
   not spend time on a SteamCMD strategy.
2. Launch: if Steam is running (check `~/.steam/steam.pid` against `/proc`, or `pgrep -x steam`):
   `steam steam://rungameid/<id>` or `steam -applaunch <id>` (test both for latency, pick the faster). If not
   running: same with `-silent` added, and consider `-nofriendsui`. Do not pass `-no-browser` or any `-cef-*`
   flag by default.
3. Launch via the `steam` binary/`steam.sh` already on PATH. Never run the game executable directly for Steamworks
   games (they would just open Steam anyway).
4. Settings, in order of default:
   - "Close Steam when I quit": default ON when Grapple started Steam (flag it so we do not close a Steam Josh
     started himself); after the session ends and no other Steam game runs, `steam -shutdown`.
   - "Keep Steam ready": default OFF; label it with the measured RAM cost.
5. Surface it honestly in the UI: "Steam will start in the background (about 1.3 GiB RAM)".

Measure before shipping (all read-only on `/proc`, easy to script as a one-off):
- Idle `-silent` Steam: PSS of `steam` + all `steamwebhelper` (sum `/proc/<pid>/smaps_rollup` `Pss:` for the
  `steam` process and its descendants) at 30 s, 5 min and 1 h after start. Compare to the 1.3 GiB figure above.
- Same during a running game (to see Valve's May 2025 reduction on this build).
- Time from "click" to the game process appearing: cold with `-silent steam://rungameid`, cold with
  `-silent -applaunch`, warm with each. Several runs each.
- Whether `-silent` still spawns the web helper and renderers (check `pgrep -a steamwebhelper` 20 s after a
  silent start).
- Optionally, cold start with `-noverifyfiles -skipinitialbootstrap` to see if it is worth the tradeoff.

Not possible or not worth it (do not build): a standalone "ownership check" (the license check is inside the
signed client and returns only through its IPC); SteamCMD `app_run` for normal games; killing `steamwebhelper`
(auto-restarts, breaks the UI the client expects); any Steamworks emulation (off the table, and VAC risk).

## Gaps / uncertainty

- Valve developer wiki pages (SteamCMD, Command line options) could not be fetched (403 / bot check). Flag
  descriptions come from search-engine snippets of those pages and from the Debian man page text.
- No hands-on test of `-no-browser`, `-silent` RAM, `-offline` or SteamCMD `app_run` (rules: do not start/stop
  Steam, install or log in). Statements about them rest on dated reports and on `strings` of the local binary.
- `-textclient` exists in the binary but I found no documentation; do not rely on it.
- The exact steam_api handshake with `steamclient.so` is not publicly documented; section 1 lists only what is
  directly observable on this machine.
