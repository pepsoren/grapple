# Invites: "come play with me", sent through tether (plan, 2026-11-25)

Josh asked: "look into inviting people through grapple to join the game. Find a way to make it possible. Those
invites should be able to be sent via tether."

Nothing here is built yet. Tags: **[code]** read in the repos (path given) · **[docs: url]** public documentation ·
**[inferred]** my reasoning, not checked · **[untested]** should work on paper, nobody has tried it.

**Short answer.**
- An invite that works for **every** game can be built entirely in grapple + tether: a game card in a DM with one
  button that starts, installs, or opens the store page for the game on the friend's computer.
- **Real joining** (landing in the sender's lobby or server) is possible for many **Steam** games, CS2 included,
  through Steam's own `steam://joinlobby` / `steam://connect` URLs.
- **Rocket League:** no outside program can put someone in a party. Honest version: launch the game for both, drop
  them in the same tether voice channel, optionally pass a private match name/password, or use Rocket League's own
  in-game Epic friend invites.

---

## 1. What exists today in tether

### 1.1 How grapple signs in

- OAuth 2.0 + PKCE for a fixed list of first-party apps; no registration, no client secret, adding an app is a code
  change. [code: `docs/apps.md`, `apps/server/src/apps/registry.ts`]
- grapple = client id `grapple`, loopback redirect `http://127.0.0.1:<port>/callback`, maximum scopes
  `identify presence.game friends.read` (`registry.ts` ~line 42); grapple asks for exactly those
  (`crates/grapple-tether/src/oauth.rs:20`). [code]
- Tokens: `tapp.` (7 days) and `tapr.` (rotating, lapses after 90 days unused); only hashes stored (`app_grants`).
  [code: `docs/apps.md` "Decisions"]
- Scopes defined today (`packages/shared/src/apps.ts` `APP_SCOPES`): `identify`, `presence.game`, `friends.read`,
  `bots` (developer site only), `guilds`, `people.search`, `dm.threads` (weave only). [code]

**Status:** tether's apps sign-in for grapple is merged and live; the invites described below are being shipped.

### 1.2 How the game is published (gateway op 3)

- An app session sends `{"op":3,"d":{"activities":[{name, type:0, timestamps:{start}, assets:{large_image}}]}}`;
  `large_image` is `steam:<appid>` or `icon:<sha256>`. The game lives only in the presence store, never in the
  database; an app session's game beats the desktop app's detected game. [code: `docs/tether-contract.md` §4,
  `docs/discord-compat.md` "Game activity", `packages/shared/src/games.ts`]
- **No rich presence.** The server keeps only `name`, `type`, `timestamps.start`, `assets.large_image`; "Anything else
  in the activity (`application_id`, `details`, `state`, `party`...) is ignored", and "Not implemented: rich presence
  (`details`, `state`, parties, buttons)". [code: `docs/discord-compat.md` ~line 1760] `packages/shared/src` has no
  `party` or `secrets` fields [code: grep]. So tether has **no Discord-style activity invites** (no `secrets.join`,
  no "Ask to join").

### 1.3 What an app token can and cannot do

Only the routes in `docs/apps.md` "Scopes"; every other route answers `401` to an app token. [code]

- **grapple can:** `GET /users/@me`, `GET /users/@me/relationships` (friends only), read/upload game icons; on the
  gateway READY with friends and presences, `PRESENCE_UPDATE`, `RELATIONSHIP_ADD/REMOVE` (`appMayReceive`,
  `packages/shared/src/apps.ts:257`), and send op 3.
- **grapple cannot:** send a DM or post any message, read messages/servers/voice state, join voice (op 4 closes with
  `4101`), or receive any other event.

**Precedent: weave already has the two powers invites need** [code: `docs/apps.md` "weave"]:
- **`dm.threads`** (`POST /users/@me/dm-threads`, `services/app-dm-threads.ts`) posts **as the person** in a 1:1 DM:
  an embed built by tether from validated fields, weave's text cleaned with `plain`/`literal` (cannot become a
  mention or link), marked `via_app: {id, name, icon}` (`packages/shared/src/api.ts:649`), not editable, gated by
  `requireCanMessage` (`services/dms.ts`; blocks/DM settings → 403 `50007` without saying which), rate limited
  (`appDmThreadPerUser`, 10 / 10 min).
- **weave notices** (`POST /apps/weave/notices`) come from a system author `~weave`: a components-v2 message with an
  "Open in weave" link button, a per-recipient switch, 20/h per recipient and 600/h total.
- An invite route should copy `dm.threads` (same checks, clean-up, `via_app`).

### 1.4 Messages, embeds, buttons, links

- Messages use Discord's shape: content, embeds, components v1 and v2. [code: `packages/shared/src/components.ts`,
  `bot-messages.ts`]
- **Link buttons and embed URLs must be `http(s)`**: `z.url({ protocol: /^https?$/ })` (`components.ts:310`,
  `:768`, `bot-messages.ts:65`). [code]
- **Exception: `tether:` links.** Server posts and notices may carry `tether:settings/...` / `tether:layout/...`
  buttons that the web app intercepts. [code: `packages/shared/src/tether-links.ts`,
  `apps/web/src/lib/tether-links.ts`, `docs/plan-notice-posts.md`]
- **Special cards** are drawn from a typed field on the message, e.g. `server_post: {kind, ...}`
  (`packages/shared/src/server-posts.ts`). A "game invite" card fits that pattern. [code]
- **tether desktop (Electron)** registers its own `tether:` scheme (`apps/desktop/src/main/index.ts:660`), and
  `decideOpen()` (`apps/desktop/src/main/security.ts:98`) only opens `https:` and `mailto:` externally. **A
  `grapple://` link is denied today**; it needs an allow-list entry handed to `shell.openExternal`. [code]
- In a browser, opening `grapple://…` shows the browser's own "Open grapple?" prompt. [inferred]
- **Steam identity:** tether already links a **verified SteamID64** per person via Steam OpenID (no API key), shown on
  the profile to people who may look the person up. So tether can tell grapple "this friend's SteamID64 is X".
  [code: `apps/server/src/connections/steam.ts`, `docs/discord-compat.md` ~line 720]
- **Voice:** only a person's own session joins voice (op 4) [code: `docs/voice.md`]. "Both in the same voice
  channel" must be a **tether client** action on each side, never grapple's.

---

## 2. How an invite travels (tether design)

### 2.1 Recommended approach

**A new grapple-only scope `invites`**, modelled on `dm.threads`. grapple sends a structured **game invite**; tether
posts it in the 1:1 DM as the sender ("via grapple"), drawn as a game card. The recipient's grapple, if signed in
with `invites`, gets a gateway event and can act on a click.

Why not Discord-style `secrets.join` in presence: presence reaches every friend and everyone in every shared server
(a join secret would leak to all of them), tether drops rich presence anyway, and an addressed invite is simpler and
private. Later, an "Ask to join" button on a friend's game can reuse the same object in reverse. [inferred]

### 2.2 Data shape (new `packages/shared/src/game-invites.ts`) [inferred design]

```jsonc
// POST /api/v1/users/@me/game-invites   (app token with `invites`, or a person's own session)
{
  "recipient_id": "123…",                  // a friend (v1); see 2.5
  "game": {
    "id": "steam:730",                     // steam:<digits> | epic:<appName> | manual:<slug>
    "name": "Counter-Strike 2",            // 1-128, cleaned like weave's text
    "image": "steam:730"                   // same rule as activity large_image (GAME_IMAGE_RE)
  },
  "join": {                                // optional; never in the message, never logged
    "kind": "steam_lobby",                 // steam_lobby | steam_connect | text
    "value": "109775241...",               // digits (lobby) | ip:port[/pw] (connect) | <=256 printable chars (text)
    "steam_id": "7656119..."               // sender's SteamID64 (steam_lobby); must equal their linked Steam
  },
  "note": "ranked?",                       // optional, 0-100, plain text
  "voice_channel_id": "…"                  // optional: the voice channel the sender is in
}
```

Answer `201 {"invite": APIGameInvite, "message_id", "channel_id"}`. `APIGameInvite` rides on the message as
`game_invite` (like `server_post`):

```jsonc
{ "id": "…", "sender_id": "…", "recipient_id": "…", "game": { "id", "name", "image" },
  "joinable": true,          // a payload exists (the payload itself is not here)
  "voice_channel_id": null,  // only if the recipient can see that channel
  "state": "sent",           // sent | accepted | declined | expired
  "expires_at": "…ISO…" }
```

- **The card is a normal message:** history, unread, push all work through `createMessage`. `content` fallback:
  "Invites you to play Counter-Strike 2".
- **The join payload lives only in memory/Redis** keyed by invite id, TTL = expiry (default **15 min**, max 1 h). Never
  in Postgres, a log or the message.
- A small `game_invites` row (ids, game id, state, `expires_at`, no payload) keeps state; each change sends
  `MESSAGE_UPDATE` so the sender's card updates.

### 2.3 Who renders it

- **tether web and desktop** draw the card: game art (`/api/v1/game-icons/...`, exists), name, "via grapple", the
  note, **one** main button and a small Decline.
- tether can't know the recipient's installed games, so the card just says **Play** and grapple decides (Play /
  Install / Get). With no grapple sign-in at all (tether knows via `activeAppBadges`, `services/apps.ts`), the card
  shows **Get grapple** instead. [inferred]
- With a visible `voice_channel_id`, the card adds **Join voice** (the tether client joins with its own session).
- Phone (`apps/mobile`): read-only card, "Open on your computer". [inferred]

### 2.4 How the click reaches grapple on the recipient's machine

1. **Gateway (preferred, no browser prompt).** The tether client calls `POST /users/@me/game-invites/:id/accept`
   (person's session). The server sends `GAME_INVITE_ACCEPTED` to the recipient's grapple app sessions with
   `invites` (a new case in `appMayReceive`). grapple fetches `GET /users/@me/game-invites/:id` with its app token
   (**only the recipient** gets `join`, only before expiry) and acts (section 3). The accept answer says
   `{"app_notified": true|false}`. grapple also gets `GAME_INVITE_CREATE` on arrival, so it can show its own toast
   with the right button even when tether isn't open.
2. **`grapple://invite/<invite id>` deep link (fallback when `app_notified: false`).** The link carries only the id.
   grapple starts, signs in as usual, fetches the invite and acts. grapple needs:
   - the Tauri deep-link plugin: Linux registers `x-scheme-handler/grapple` via the `.desktop` file, Windows a
     registry entry [docs: https://v2.tauri.app/plugin/deep-linking/];
   - single-instance with its `deep-link` feature so a second launch hands the URL over [docs: same page] [untested];
   - a `handle_deep_link()` in `app/src-tauri/src/main.rs` next to the existing `--launch <game id>` handoff
     (`launch_arg` / `launch_from_args`, single-instance callback) [code];
   - tether desktop allowing `grapple:` in `decideOpen` (§1.4).
   - **Naming:** `main.rs` already registers `register_asynchronous_uri_scheme_protocol("grapple", art::handle)` for
     in-webview art. That should not clash with an OS handler, but it's confusing [inferred]. Josh decides:
     `grapple:` or e.g. `grapple-app:`.

A deep link never starts a game silently: it opens grapple on the invite with one **Play** button, so a link
elsewhere on the web can't launch games. The gateway path may act directly, since the click was in tether's own
card. [inferred, recommended]

### 2.5 Anti-abuse

- **Who (v1):** friends only, plus `requireCanMessage` (no block either way, recipient's DM settings, no bots /
  system / deleted accounts) → 403 `50007` without saying which. Later maybe "invite the voice room". [inferred]
- **Rate limits** (new buckets like `appDmThreadPerUser`): 10 per 10 min per sender, 3/h per sender→recipient pair,
  20/h per recipient (counted before any lookup, like weave's).
- **Recipient setting "Game invites":** Friends / Off, plus "Mute this game" from the card menu, enforced by the
  server. Muting the DM mutes push only.
- **Expiry:** past `expires_at` the card shows Expired and loses its button (a sweep marks rows, sends
  `MESSAGE_UPDATE`); the payload is already gone by TTL.
- **Validation:** game name and note through weave's `plain`; `game.image` against `GAME_IMAGE_RE`; `join.value` per
  kind: digits only (lobby), `IPv4:port[/password]` (connect), printable <256 with no control characters (text).

### 2.6 Privacy

- A payload can reveal a server IP, lobby id or private match password. It goes only to the recipient, only through
  the authenticated `GET`, only until expiry; never in the message, presence, logs, push text or data export.
  Request logs already drop query strings [code: `docs/apps.md` "Privacy"]; the payload is in a JSON body that is
  never logged.
- The sender sees what will be shared before sending ("Shares lobby", "Shares server address").
- The invite itself (game, people, state) is an ordinary DM message, kept and deleted like any message.

---

## 3. What "join" can really do, per platform

### 3.1 Steam (CS2 and other Steam games)

**URL forms**
- `steam://connect/<IP>[:<port>][/<password>]`: connect to a server; DNS names no longer work. [docs:
  https://developer.valvesoftware.com/wiki/Steam_browser_protocol (read via the Wayback Machine)]
- `steam://run/<appid>//<args>/`: run (install if needed) with launch arguments. [docs: same]
- `steam://joinlobby/<appid>/<lobbyid>/<steamid64>`: only on the wiki's talk page as undocumented. Steam's own UI
  builds exactly this for "Join Game"; the third part is the **friend being joined**. [code, Valve's shipped client
  JS: https://github.com/SteamDatabase/SteamTracking/blob/master/ClientExtracted/steamui/ui.js]
- `steam://rungame/<appid>/<friend64>/<urlencoded connect string>`: also undocumented; Steam's UI uses it for a
  friend with a rich-presence `connect` string and no lobby. [same ui.js]
- grapple already hands URLs to Steam with `Steam::open_url()` (`crates/grapple-launch/src/steam.rs:170`), so
  **running a join URL needs no bridge**. [code]

**What Steam does on join**
- A lobby invite accepted with the game closed launches it with **`+connect_lobby <lobby id>`**. [docs:
  https://partner.steamgames.com/doc/api/ISteamMatchmaking]
- A game/rich-presence invite appends the connect string to the command line; a running game gets
  `GameLobbyJoinRequested_t` / `GameRichPresenceJoinRequested_t`. [docs:
  https://partner.steamgames.com/doc/api/ISteamFriends]

**Where the sender's lobby or server can be read**
- **Web API `GetPlayerSummaries`:** needs an API key; game fields only for **public** profiles: `gameid`,
  `gameserverip` (`0.0.0.0:0` when not on a server), `gameextrainfo`. [docs:
  https://developer.valvesoftware.com/wiki/Steam_Web_API] `lobbysteamid` / `gameserversteamid` appear in real
  responses but no Valve page documents them [untested]. Would mean a Steam Web API key on the tether server plus
  polling Valve. **Not recommended:** the bridge gives the same data locally.
- **Rich presence `connect` key:** a game sets it so friends get "Join Game". [docs: ISteamFriends `SetRichPresence`]
  Only games that set it can be joined this way.
- **Through grapple's Steam bridge.** Names from public typings, none called by grapple yet [untested]; sources:
  https://unpkg.com/@decky/ui@5.0.2/src/globals/steam-client/Friends.ts,
  https://unpkg.com/@steambrew/client@5.8.5/src/globals/steam-client/Friends.ts:
  - `SteamClient.Friends.InviteUserToGame(steamId: string, appId: number, connectString: string): Promise<boolean>`
  - `SteamClient.Friends.InviteUserToLobby(steam64Id: string, steamIdTarget: string): Promise<boolean>`. Valve's UI
    calls it as `(friend64, lobby64)` [code: ui.js]; the second argument's meaning is [inferred].
  - `SteamClient.Friends.InviteUserToCurrentGame(steam64Id, steamIdTarget)`
  - Global `friendStore.GetFriendState(accountId)` → `CPlayer`; `.persona` has `m_gameid`, `m_game_lobby_id`,
    `m_mapRichPresence`, `m_unGameServerIP/Port`, getters `connect_string`, `is_in_valid_lobby`,
    `is_in_joinable_game`. [docs: https://unpkg.com/@decky/ui@5.0.2/src/globals/stores/friendstore/shared.ts]
  - **No `JoinGame` function.** Valve's "Join Game" builds the URL above and navigates or calls
    `SteamClient.URL.ExecuteSteamURL(url)`. [code: ui.js]
  - `SteamClient.Apps.RunGame(gameId: string, launchOptions: string, -1, launchSource)`; extra options go before the
    user's own. [docs: @decky/ui `App.ts`]

**So: real Steam invite? Own lobby? What does the recipient need?**
- **Real Steam invite:** probably yes via the bridge (`InviteUserToGame(friend64, appid, "")` or
  `InviteUserToLobby(friend64, lobby64)`) [untested]. Needs Steam friends (or a shared group): InviteUserToGame
  "returns false if the target is not a friend" [docs: ISteamFriends]. The recipient sees Steam's invite in Steam's
  UI, which grapple hides. **An extra, not the main path.**
- **Sender's own lobby/server:** probably readable via `friendStore` (own `CPlayer`) [untested].
- **Best path when both are Steam friends** [untested]: send nothing. The **recipient's** bridge reads the **sender's**
  persona from its own `friendStore` (SteamID64 from tether's verified link), builds `joinlobby`/`rungame` and hands
  it to Steam. No payload travels, nothing can be faked, Steam decides joinability.
- **Not Steam friends:** the sender's grapple reads its own lobby id and sends `join.kind = steam_lobby`. Joining works
  only if the lobby isn't friends-only (Private = invite only, FriendsOnly = friends + invitees, Public/Invisible =
  anyone with the id). [docs: ISteamMatchmaking]

**Bridge safety.** Today every bridge call is a fixed template with integer/boolean arguments only, enforced by
`is_template()` (`crates/grapple-download/src/steam_bridge.rs` lines 32–137, 204–211). [code]
- SteamID64s and lobby ids exceed 2^53, so they don't fit a JS number: they need a new template kind, a **quoted
  string of 1–20 digits**, allowed by `shape()`. [inferred]
- **Connect strings are free text: never into the bridge.** Build `steam://rungame/.../<encoded>` in Rust and run it
  via `Steam::open_url()` (a process argument, never JS).
- Reading `friendStore` is a read-only template returning JSON, e.g. `friendStore.GetFriendState(<digits>).persona`
  mapped to `{gameid, lobby, ip, port, connect}`. [inferred]
- Invites over the bridge stay **opt-in** under the existing "Steam bridge" switch (Josh decides).

**CS2**
- Parties are Steam lobbies: joining one is `steam://joinlobby/730/<lobby>/<friend64>`, Valve UI's generic path
  [inferred]. Cold-start `+connect_lobby` in CS2 is [untested] but Valve's invite path relies on it.
- Lobby privacy (`lobby_default_privacy_bits2`, 1 = friends only) decides whether a non-friend can join by id. [docs:
  https://totalcsgo.com/commands/lobbydefaultprivacybits2 (third party)]
- Community servers: `steam://connect/<ip:port>[/pw]` [docs: VDC] or `+connect <ip:port>` [inferred].
- Competitive/Premier party max 5 [docs: press only,
  https://www.dust2.in/news/57800/valve-relaxes-cs2-premier-queues-rank-bandwidth]; the game refuses a 6th.
- In-game "friend code" invites exist for non-friends [untested, video only].

### 3.2 Epic / EOS and Rocket League

- **EOS invites** (Presence `JoinInfo` via `EOS_PresenceModification_SetJoinInfo`, `EOS_Lobby_SendInvite`, Session
  invites, Custom Invites ≤500 chars) all run **inside the game's process** under its EOS credentials and logged-in
  user. Overlay Join fires a callback in the game, which calls `EOS_UI_AcknowledgeEventId`. [docs:
  https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/social-overlay/sdk-integration,
  https://dev.epicgames.com/docs/epic-online-services/player-and-game-data/custom-invites-interface]
- The only outside party is the Epic Games Launcher, which can **accept** a mapped invite (it just starts the game;
  the game picks the invite up after login). [docs:
  https://dev.epicgames.com/docs/epic-games-store/services/launcher-invites]
- No public web API for presence/lobbies/sessions/invites. The EOS Web API has read-only Friends for your own EOS
  product and says "Other undocumented APIs ... should never be called directly". [docs:
  https://dev.epicgames.com/docs/web-api-ref/web-api-introduction] → **grapple cannot create or accept an Epic
  invite.** [docs + inferred]
- **Launch:** the EGL URL `com.epicgames.launcher://apps/<ns>%3A<item>%3A<app>?action=launch&silent=true` documents
  only `action` and `silent`: no argument passthrough, no join action. [docs:
  https://dev.epicgames.com/docs/epic-games-store/protocol-activation] grapple builds legendary-style args itself
  (`crates/grapple-launch/src/epic.rs`, `legendary::game_args`), so it can add launch args [code]; Rocket League has
  no known party-join arg [docs: none found].

**Rocket League**
- Parties use Epic friends in the game (Friends list, Party, Notifications); cross-play on by default. [docs:
  https://www.epicgames.com/help/en-US/rocket-league-c182/basics-c190/how-do-i-send-and-receive-friend-requests-in-rocket-league-a10412]
  Whether parties are EOS lobbies or Psyonix's own (PsyNet), and whether EOS overlay Join works for them, is **not
  public** [untested].
- **What works from outside:**
  1. Both launch Rocket League; the sender invites in game via Epic friends (needs Epic friendship, nothing grapple
     can automate).
  2. **Private match** by name + password: the sender creates it in game, the invite carries `join.kind = text`
     ("Private match · name / pass"), the recipient's Play launches and copies it to the clipboard; they type it under
     Play › Private Match › Join. [docs: https://www.epicgames.com/help/c-37599050/a24641297]
- **EOS overlay under Proton:** Epic says "MacOS, Linux, and Steam Deck are not currently supported" [docs:
  https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/eos-overlay-overview]. Community reports are
  mixed (legendary/Heroic install it into the prefix; some invite buttons do nothing; recent GE-Proton fixes) [docs:
  https://github.com/derrod/legendary,
  https://www.gamingonlinux.com/2025/05/ge-proton-10-2-brings-some-essential-fixes-for-windows-games-on-linux-steamos-steam-deck].
  grapple already turns it on per game (`crates/grapple-launch/src/eos.rs`) [code]. **Whether Shift+F3 › Invite/Join
  works in Rocket League on Josh's machine is [untested]**: try it once with two accounts.
- Easy Anti-Cheat is required on PC from 2026-04-30, Linux supported [docs:
  https://www.gamingonlinux.com/2026/04/rocket-league-adds-easy-anti-cheat-with-steam-deck-linux-still-supported];
  nothing here touches the game process [inferred].

### 3.3 Hand-added and other games

- **Always:** "launch the same game": grapple starts `manual:<slug>` or shows **Get**. Hand-added ids are per
  computer, so match by **name** as well as id. [inferred]
- **Optional per-game "join template"** in grapple's game settings: arguments with `{address}` (e.g. Minecraft Java's
  Quick Play `--quickPlayMultiplayer {address}` [inferred: from memory, not checked against a source; untested]), or
  "copy to clipboard". The sender types the address (or grapple remembers the last), sent as `join.kind = text`.
- The payload is used **only** as one argument inside the recipient's **own** template: it never picks the program,
  and is validated (no control characters, <256, no leading `-`). [inferred, recommended]

### 3.4 The fallback that always works

The recipient gets the card → one click: launch if installed, start the install if owned, else open grapple's store
page (cheapest store) → **Join voice** if the sender was in a tether voice channel → they party up in game by hand.
This is v1; everything later builds on it.

---

## 4. Build order

### v1 — "Invite to play", every game

- **tether server:** scope `invites` in `APP_SCOPES` + `SCOPE_TEXT` ("Send game invites to your friends, as you";
  never-line like `dm.threads`); grapple's registry scopes add it. `game_invites` table + migration. Routes `POST
  /users/@me/game-invites` (app token with `invites` or session), `GET .../:id` (sender or recipient; `join` for the
  recipient only), `POST .../:id/accept|decline` (recipient session or app token). Message carries `game_invite` +
  `via_app`, not editable. `appMayReceive`: `GAME_INVITE_CREATE` / `GAME_INVITE_ACCEPTED` for the recipient's
  `invites` sessions. Expiry sweep, rate-limit buckets, "Game invites" setting, payload store (memory/Redis TTL; v1
  uses only `text`).
- **tether clients:** `GameInviteCard` in web/desktop (sender + recipient views, states, Join voice); desktop
  `decideOpen` allows `grapple:`; Settings › Privacy switch; phone read-only card; i18n in all ten languages.
- **grapple:** `grapple-tether`: `SCOPES` adds `invites`; `client.send_invite()` / `get_invite()`; `gateway.rs` turns
  the two events into `TetherEvent`s; a sign-in without the scope (403 50001) shows **Allow invites** (re-consent).
  App: deep-link plugin + single-instance `deep-link`, `grapple://invite/<id>` in `main.rs` next to `--launch`;
  invite toast; Play / Install / Get via existing launch, download and store code.
- **Tests (no real accounts):** tether vitest like `apps/server/test/weave-dm-threads.test.ts` (friends only, blocks,
  limits, expiry, payload only to recipient and never in the message, `appMayReceive`) + card tests like
  `apps/web/src/dm/weave-chat.test.tsx`. grapple: the fake tether in `crates/grapple-tether/tests/common/mod.rs` grows
  the routes/events; unit tests for deep-link parsing (reject anything but `invite/<digits>`).
- **Josh by hand:** `pnpm dev:lite --seed`, two seeded accounts, two grapple instances (or grapple + tether web): send,
  receive, Play / Install / Get, decline, expire; desktop link with grapple closed and running; once on Windows.
- **Josh decides:** new scope (everyone presses Allow again); as the person "via grapple" (recommended) or from a
  `~grapple` system author; scheme name; 15-minute expiry; friends only.

### v2 — Steam join (lobby / server)

- **tether:** accept `steam_lobby` / `steam_connect`; for `steam_lobby` check `steam_id` = the sender's linked Steam.
  In `GET`, give the recipient the **sender's** linked SteamID64 even without a payload, only if the recipient may
  see the sender's profile connections (Steam is already `visibility: 1` [code], so nothing new leaks [inferred]).
- **grapple:** `steam_bridge.rs` gains the digit-string template kind, a read-only `FriendPersona(<digits>)` call via
  `friendStore`, optionally `InviteUserToLobby` / `InviteUserToGame` with an empty connect string; each behind
  `Call::needs` so a Steam that renamed something just turns the feature off. Recipient order: (1) bridge reads the
  sender's persona if Steam friends → `joinlobby` / `rungame` / `connect` URL; (2) else the invite payload; (3) else
  `steam://rungameid/<appid>`. URLs always run via `Steam::open_url()`. Sender: "Share lobby" is on when the bridge
  can read its own lobby/server.
- **Tests:** fake CDP fixtures in `crates/grapple-download/tests/steam_bridge.rs` (persona with/without lobby, renamed
  functions); template tests (digits only, quotes/backslashes rejected); URL builder tests (encoding, port 27015).
- **Josh by hand (CS2 + a second Steam account):** (a) Steam friends: join by persona; (b) not friends, public lobby:
  join by id; (c) friends-only lobby → refused, card says **Can't join**; (d) community server via
  `steam://connect`; (e) cold start: does `+connect_lobby` land in the lobby? Then one other Steam co-op game.
- **Josh decides:** bridge for invites (opt-in, recommended); no Steam Web API key on tether (recommended); whether
  grapple also sends real Steam invites.

### v3 — Epic / Rocket League (what is realistic)

- **grapple only** (no tether change beyond v1): Rocket League's invite offers "Private match" fields (name +
  password, typed by the sender, `join.kind = text`); the recipient's Play launches and copies them. An "Epic invites
  in game" icon hint when both have the EOS overlay on.
- **Josh by hand:** does Shift+F3 open in Rocket League with grapple's overlay setting? Does Invite/Join there work
  between two Epic accounts? If yes the card can say "Join from Epic overlay"; if not, private match only.
- **Not doable:** joining a Rocket League party from outside the game, or sending an Epic invite from grapple. (§3.2)

---

## 5. UI sketch (for a later mockup)

Short labels, no explanatory sentences.

- **Where Invite lives in grapple:** game page **Invite** beside Play (person-plus icon); right-click a game
  **Invite…**; now-playing bar **Invite** icon button; friends strip hover **Invite** for the game you're in, or
  **Join** if the friend plays something you own and it's joinable (v2).
- **Friend picker (popover):** search; friends sorted online → same game → rest; row = avatar, name, small game icon
  if playing; multi-select up to 4 (one invite per friend, no group card in v1). Below: **Share lobby** toggle (v2,
  only when there's something to share); **Address** or **Match / Pass** field (games with a join template, Rocket
  League); **Note** (optional, 100 chars). Button **Send**.
- **tether card, recipient:** square art, game name, "via grapple", the note in quotes. Main button **Play** / **Join**
  (v2) / **Get grapple**. Secondary **Join voice** (if offered), **Decline**, ⋯ (**Mute this game**, **Turn off
  invites**). Small line: "Lobby" / "Server" when a payload exists, and time left ("12 min").
- **tether card, sender:** same art and name, "You invited Maya", the state.
- **grapple toast (recipient):** avatar + game art, "Maya · CS2", **Play** / **Join**, ✕; expires with the invite.

| State | Sender sees | Recipient sees |
| --- | --- | --- |
| sent | Sent | Play / Join |
| accepted | Joined* | Launching… → Playing |
| declined | Declined | Declined |
| expired | Expired | Expired (no button) |
| not owned | Sent | Get · <price> (grapple picks the store) |
| no grapple | Sent | Get grapple |
| can't join (v2) | Sent | Can't join · Play |

\* "Joined" only means grapple launched the game after Play/Join; nobody can confirm they landed in the same lobby.
Josh may prefer **Launched**.

---

## 6. Open decisions for Josh

1. **New `invites` scope:** every grapple user presses Allow once more (grapple shows **Allow invites**).
2. **Who the message is from:** the sender "via grapple" (like `dm.threads`, recommended) or a `~grapple` system
   account (like weave notices).
3. **Who may be invited:** friends only for v1, or also the voice channel / server.
4. **Expiry:** 15 min default, 1 h max?
5. **Deep-link scheme:** `grapple:` (also the webview art protocol's name) or another.
6. **Steam bridge for invites:** opt-in under the existing switch? Also send real Steam invites?
7. **No Steam Web API key on tether** (recommended), or keep `GetPlayerSummaries` as a fallback without the bridge.
8. **"Joined" or "Launched"** for an accepted invite.
9. **Rocket League:** accept "launch + voice + private match name/pass" as the product, and test the EOS overlay
   invite by hand.
