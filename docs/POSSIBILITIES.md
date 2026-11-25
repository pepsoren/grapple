# What grapple could do later (2026-10-19)

Answers to Josh's "what is possible" questions. Nothing here is planned yet. "Test first" means it
should work on paper but hasn't been tried.

## 1. Game invites and joining a friend's game

**Invite to play: yes, for every game.**
- grapple sends a game card through tether, in a DM or a voice channel.
- The card shows what the friend needs to do next: **Play** if it's installed, **Install** if they
  own it, **Buy** (the cheapest store) if they don't.
- One click from tether opens grapple through a `grapple://` link and starts it.
- This is a tether + grapple feature, so it works whatever store the game comes from.

**Join a friend's game: partly, Steam only.**
- When a Steam game exposes where you are, Steam's public profile data includes it: a lobby id or
  a server address (`GetPlayerSummaries`: `lobbysteamid`, `gameserverip`).
  - Most games with Steam lobbies do this, as do Source-style server games (CS2, TF2, Garry's Mod)
    and many co-op games.
  - Each player's grapple reads its own player's entry through tether's server, which holds one
    Steam API key. That needs a public Steam profile.
  - The entry is shared in the tether presence. A **Join** button then runs
    `steam://joinlobby/<app>/<lobby>/<steamid>` or `steam://connect/<ip>`. Steam drops you
    straight in, with no Steam window.
- Games that keep their lobbies to themselves expose nothing, and nobody outside the game can
  join them.
- Epic: no. Epic's invites only exist inside each game (EOS), and Epic's friends system isn't
  open.

**Sending a real in-game invite: no.** Only the game itself can send one, from inside its own
process (Steam's or Epic's SDK). grapple can't inject into it. "Invite to play" plus "Join" covers
most of the same ground.

## 2. Adding someone you played with

**Possible for Steam games, opt-in.**
1. tether gets "Link Steam": Steam's own sign-in page proves which Steam account you own
   (OpenID). It only hands over your Steam ID; no password is involved.
2. Steam keeps a "Recently played with" list
   (`steamcommunity.com/my/friends/coplay`). Many multiplayer games fill it in.
   - grapple can read it with the Steam sign-in that its store window already keeps.
   - Reading those cookies from the app needs care; test first.
3. grapple asks tether which of those Steam IDs belong to tether users who allow it. They show
   up as "Played with you in Rust · Add on tether".

**Fallback when the coplay list is empty:** two grapple users in the same Steam lobby or on the
same server (point 1's lobby/server id) can be matched by tether's server. Only lobby/server ids
are compared, never locations. Being in the same game at the same time isn't enough, because
there are too many players.

Epic games and games without lobbies are out of reach.

## 3. Tying tether and grapple together

**Already done:** sign-in, now playing, friends, profile picture, the grapple badge.

**Presence**
- The tether profile shows the game art, how long you've been playing, party size, and the
  Join/Invite buttons from point 1.

**Voice channels**
- **Play together:** pick a game in a voice channel and everyone's grapple launches it, or
  offers install/buy.
- **What can we play?** The games everyone in the call owns, sorted by who played them most.
- When a friend launches the same game you're in, a quiet "Join voice?"

**Profile**
- A grapple section on your tether profile: top games, total playtime, recently played,
  showcased Steam achievements.

**Activity (opt-in)**
- Started a game, got an achievement, bought something.
- Shown on profiles or as a channel feed.

**Clips and screenshots**
- grapple watches Steam's screenshot folder.
- It can also keep a replay buffer with gpu-screen-recorder (works on NVIDIA + Wayland).
- One key saves the last 30 s; **Share** posts it to a tether channel.

**Overlay over games**

grapple runs on Windows and on every Linux desktop, so there's no single method. The overlay is
an always-on-top window that grapple opens itself, the same idea everywhere. Nothing is injected
into the game. Two modes: toasts (messages, invites, "clip saved", no focus taken) and a panel on
a hotkey (chat, friends, Join, clips; it takes focus, so the game lets go of the mouse).

| Platform | How | Over fullscreen games? |
| --- | --- | --- |
| Windows | A transparent topmost window | Yes for borderless and windowed games (most modern games, and Windows' default "fullscreen optimizations" behave the same). True exclusive fullscreen hides it, as it hides Discord's out-of-process overlay. Test first. |
| Linux: KDE, Hyprland, Sway, COSMIC (Wayland) | A layer-shell surface on the overlay layer | Yes: the compositor composites every game, Proton/XWayland included. |
| Linux: GNOME (Wayland) | GNOME doesn't offer layer-shell, and apps can't put themselves on top | No panel over games. Toasts become normal desktop notifications. |
| Linux: X11 | An always-on-top window | Yes while compositing; if the desktop stops compositing fullscreen games, it's hidden. |

- Steam games on every platform still have the Steam overlay (Shift+Tab), since Steam runs in
  the background.
- **The other way: draw inside the game's frames.** This is how Overwolf, Steam, Discord's old
  overlay and RTSS do it.
  - How: a DLL loaded into the game hooks the frame "present" call (DirectX 9–12, OpenGL,
    Vulkan) and draws the overlay onto each frame. Hooking the game's window messages catches
    mouse and keyboard while the overlay is open.
  - Works in true exclusive fullscreen.
  - Overwolf can do it in anti-cheat games because anti-cheat vendors and game developers
    approved it and allow its signed DLLs. Without that approval, injecting on Windows risks
    kicks or bans (EAC, BattlEye); Vanguard blocks it outright.
  - On Linux it's simpler than on Windows. Proton turns DirectX into Vulkan, so one official
    Vulkan layer (like MangoHud's) covers nearly every Proton game plus native Vulkan games;
    OpenGL games need a second hook. A Vulkan layer is a normal loader feature, not a hack,
    and Proton's anti-cheat generally tolerates MangoHud.
  - Input is the hard part on Linux: the game reads input straight from the desktop.
  - A possible mix: a display-only Vulkan layer for toasts (works on GNOME too); the
    interactive panel stays a window.

**Your stuff on every machine**
- tether's server stores grapple settings, collections, hidden games, playtime and wishlist.
- Signing in on a new machine restores them.

**Wishlists and gifts**
- See friends' wishlists.
- "Lerato's wishlisted game is 60% off". The gift itself goes through Steam's real checkout.
- "3 friends wishlisted this" for a group buy.

**Bots**
- A tether bot posts "Free on Epic this week" and big sales on group wishlists to a channel.

**Game servers on the home server (ambitious)**
- Start a Valheim, Minecraft or ARK dedicated server on the box tether runs on, straight from
  tether.
- Friends see it and click Join.

## 4. Normal launcher features: what's possible

| Feature | Possible? | Notes |
| --- | --- | --- |
| One library, launching without launchers | Yes (done) | Epic fully launcherless. Steam games need Steam running hidden: its DRM lives in the Steam client. |
| Playtime | Yes (done) | |
| Downloads, updates, queue | Yes (building) | Epic by grapple; Steam by Steam, hidden. |
| Uninstall, verify/repair, move install | Epic yes; Steam partly | Steam's own `steam://uninstall` and `validate` may show a Steam dialog. Test first, as with installs. |
| Cloud saves | Yes | Steam does its own while running hidden. Epic: port legendary's save sync. |
| Achievements | Steam yes, Epic unclear | Steam Web API (public profile). Epic needs its own research. |
| Proton per game | Yes | Pick a GE-Proton version (grapple downloads it), environment variables, launch options. |
| MangoHud / gamemode / gamescope toggles per game | Yes | Wrappers around the launch command. |
| Linux compatibility badges | Yes | ProtonDB rating and Steam Deck status on game and store pages. Anti-cheat status from areweanticheatyet. |
| "How long to beat" | Yes | HowLongToBeat data (unofficial). |
| GOG, itch.io, Amazon games | Yes | All have open launcherless tools: gogdl, butler, nile. |
| Any .exe / emulators / ROMs | Yes | Add a game by hand; RetroArch cores for ROM folders. |
| Mods | Later | Thunderstore / Nexus integration, r2modman-style. |
| Controller couch mode | Yes (later) | Gamepad navigation, full-screen layout, DualSense. |
| Battle.net, EA, Ubisoft | Launch only | Their DRM needs their launcher running (under Wine), like Steam. They can't be skipped. |
| Steam friends chat inside grapple | No | No public API. tether is the chat. |
| Epic friends / invites | No | Not open. |
| In-game invites sent by grapple | No | Only from inside the game. |
| Kernel anti-cheat games (Valorant, etc.) | No | They don't run on Linux at all. |
| Games whose anti-cheat isn't enabled for Linux | No | Only the developer can switch it on. grapple can warn before you buy. |
| Xbox / Game Pass PC | No | Not on Linux. |
| Steam refunds, family sharing | Link only | They open Steam's own pages. |
| Changing your in-game name | No (decided) | |
