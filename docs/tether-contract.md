# tether contract for Grapple

What Grapple (crate `grapple-tether`) relies on from tether. The tether side is on projectx's `main`
(the `grapple-apps` branch that brought sign-in was merged into it; `docs/apps.md` there explains the
why; `packages/shared/src/apps.ts` holds the shared shapes). Game invites (section 7) are on the
projectx branch `grapple-invites` until Josh merges and deploys it. If this file and the tether code
disagree, the tether code wins and this file is the bug.

Conventions: all times are milliseconds since the Unix epoch unless named `*_at` (ISO 8601) or
`expires_in` (seconds). Snowflake ids are decimal strings. Nothing below may ever be logged by
Grapple: access tokens, refresh tokens, codes, PKCE verifiers, `state`.

## 1. The instance

One origin serves everything (the web page, the API and the gateway), e.g. `https://<host>` in
production or `http://localhost:<LITE_WEB_PORT>` for `pnpm dev:lite` (Vite proxies `/api` and
`/gateway` to the server).

- API base: `<origin>/api/v1`
- `GET /api/v1/gateway` → `{"url": "wss://<host>/gateway"}` (no auth). Connect to
  `<url>?v=1&encoding=json`.
- `GET /api/v1/instance` → `{"name": "tether", ...}` (no auth). Use `name` when showing the product
  name; never hard-code it.

## 2. Signing in (OAuth 2.0 authorization code + PKCE, loopback redirect, RFC 8252)

The client id is the string `grapple`. There is no client secret.

1. Listen on `127.0.0.1:0` (IPv4 loopback literal; never `localhost`). Note the port (it is always
   1024–65535 on Linux).
2. Make `state` = 32 random bytes, base64url, and a PKCE `code_verifier` = 32 random bytes, base64url
   (43 characters, allowed set `[A-Za-z0-9._~-]`, 43–128 long). `code_challenge` =
   base64url-no-padding(sha256(ascii(code_verifier))) (43 characters).
3. Open the system browser (`xdg-open`) at:

   ```
   <origin>/oauth2/authorize
     ?client_id=grapple
     &response_type=code
     &redirect_uri=http://127.0.0.1:<port>/callback
     &scope=identify presence.game friends.read invites
     &state=<state>
     &code_challenge=<challenge>
     &code_challenge_method=S256
   ```

   (URL-encode each value.) `redirect_uri` must be exactly `http://127.0.0.1:<port>/callback`: no
   trailing slash, no query, `127.0.0.1` not `localhost`. `scope` is space-separated, must include
   `identify`, and may only contain the four scopes above (`invites`: game invites, section 7;
   a tether without section 7 refuses a request that names it, so Grapple asks for it only once
   tether has it). `state` must be 8–256 characters. Only
   `S256` is accepted.

4. The person signs in (if needed) and presses **Allow** or **Cancel** on the page "Allow Grapple
   to show what you play and see what your friends play?". The browser is then sent to one of:

   - `http://127.0.0.1:<port>/callback?code=<code>&state=<state>`
   - `http://127.0.0.1:<port>/callback?error=access_denied&state=<state>`

   A broken request (unknown client, bad redirect, bad scope, no S256) is shown as an error page in
   the browser and **never** comes back to the listener, so the listener needs a timeout (Grapple
   uses 5 minutes).

   Grapple must: accept only `GET /callback`, compare `state` with its own in constant time and ignore
   any request whose `state` differs, answer with a small `text/html` page ("You are signed in to
   Grapple. You can close this tab." / "Not signed in."), then close the listener.

5. Trade the code within **2 minutes** (it works **once**; any failed try uses it up):

   ```
   POST <origin>/api/v1/oauth2/token
   Content-Type: application/x-www-form-urlencoded

   grant_type=authorization_code&client_id=grapple&code=<code>
     &redirect_uri=http://127.0.0.1:<port>/callback&code_verifier=<verifier>
   ```

   `200` →

   ```json
   {
     "access_token": "tapp.<43 base64url>",
     "token_type": "Bearer",
     "expires_in": 604800,
     "refresh_token": "tapr.<43 base64url>",
     "scope": "identify presence.game friends.read"
   }
   ```

   `scope` is what was granted (space-separated, may be fewer than asked if fewer were asked).

### Token lifetime and refresh

- The access token works for `expires_in` seconds (7 days). The refresh token lapses after **90 days
  unused**.
- Refresh **before** the access token runs out. Grapple refreshes when less than a day is left:
  before every gateway connect, before REST calls, and every 6 hours while connected (a live
  connection does not need a fresh token, but RESUME and REST calls do, and the refresh token must
  not lapse). A refresh works with an access token that has already run out:

  ```
  POST <origin>/api/v1/oauth2/token
  Content-Type: application/x-www-form-urlencoded

  grant_type=refresh_token&client_id=grapple&refresh_token=<refresh token>
  ```

  `200` → the same shape as above with a **new access token and a new refresh token**. The old pair
  stops working at once. **Store the new pair before using it.** Presenting an old refresh token again
  revokes the whole sign-in (it means the token was copied), so never retry a refresh with a token
  that already succeeded.
- On `invalid_grant` from a refresh, the sign-in is gone: delete the stored tokens and show "Sign in
  with tether" again.
- Signing in again while signed in: store the new pair, then revoke the old refresh token (best
  effort), so tether's "Connected apps" keeps one row per computer.
- Storage: one Secret Service keyring entry, service `grapple`, user `tether:<origin>`, holding JSON
  `{access_token, refresh_token, expires_at (unix s), scopes}`. Never a file, never a log.

### Token endpoint errors

`400` with `{"error": "<code>", "error_description": "<text>"}` (RFC 6749 section 5.2):

| `error`                  | When                                                                       |
| ------------------------ | -------------------------------------------------------------------------- |
| `invalid_request`        | missing or malformed parameter, a parameter given twice                    |
| `invalid_client`         | `client_id` is not `grapple`                                               |
| `invalid_grant`          | code unknown/used/expired, wrong verifier, other `redirect_uri`, bad or old refresh token, lapsed sign-in |
| `unsupported_grant_type` | `grant_type` other than `authorization_code` / `refresh_token`            |

`429` (30 requests per 5 minutes per address) is tether's usual body: `{"code": 90003, "message":
"...", "retry_after": <seconds>, "global": false}` with a `Retry-After` header.

### Signing out

```
POST <origin>/api/v1/oauth2/token/revoke
Content-Type: application/x-www-form-urlencoded

token=<access or refresh token>&client_id=grapple
```

Always `200` (empty body), even for an unknown token (RFC 7009). The whole sign-in goes, and its
gateway connections close with `4004`. Then delete the tokens from the keyring. The person can also
sign Grapple out from tether: Settings › Connections › Connected apps › Sign out (same effect;
Grapple sees `4004` on the gateway and `401` on REST).

## 3. REST with the app token

Header: `Authorization: Bearer <access token>`. Only these routes accept it; every other route answers
`401`. A route below answers `403` `{"code": 50001, "message": "Missing Access"}` if the sign-in lacks
the scope. An expired or revoked token is `401` `{"code": 40001, "message": "401: Unauthorized"}`
(refresh once, then sign in again).

| Route                                    | Scope                         | Answer                                                                                 |
| ---------------------------------------- | ----------------------------- | -------------------------------------------------------------------------------------- |
| `GET /users/@me`                         | `identify`                    | public user `{id, username, global_name, avatar, ...}` (no email)                      |
| `GET /users/@me/relationships`           | `friends.read`                | friends only: `[{id, type: 1, user: {...}, nickname: null, since}]`                    |
| `HEAD /game-icons/:sha256`               | `presence.game` or `friends.read` | `200` if the server has it, `404` if not                                          |
| `GET /game-icons/:sha256`                | `presence.game` or `friends.read` | `image/webp`, 128 px square                                                      |
| `GET /game-icons/steam/:appId`           | `presence.game` or `friends.read` | `image/webp` 128 px (the server fetches Steam's once), `404` if none             |
| `PUT /game-icons/:sha256`                | `presence.game`               | upload, see below                                                                      |

Avatars need no token: `GET /api/v1/avatars/<user id>/<avatar hash>` (the user's `avatar` field;
null means none, draw the initial).

### Game icons for non-Steam games

1. Make a PNG of the game's icon, **at most 64 KB** (Grapple: 128×128 or smaller).
2. `sha256` = lowercase hex SHA-256 of exactly those PNG bytes.
3. `HEAD /api/v1/game-icons/<sha256>`: `200` means it is there already; skip to 5.
4. `PUT /api/v1/game-icons/<sha256>` with `Content-Type: image/png` and the raw bytes. `204` on
   success. Errors: `400` `{"code": 50035, "errors": {"icon": ...}}` (the hash doesn't match, or not a
   PNG), `413` (over 64 KB), `429` (30 uploads an hour per person). The server keeps its own 128 px
   WebP re-encode named by the hash, so an upload can never overwrite anything.
5. Use `icon:<sha256>` as `assets.large_image` in the activity. A failed upload is not fatal: send the
   activity without `assets`.

Steam games never upload: use `steam:<appid>` (digits only).

## 4. Gateway

JSON text frames `{"op": n, "d": ..., "s": seq|null, "t": "EVENT"|null}`. At most 16 KB per frame and
120 frames a minute from the client.

1. Connect to `<gateway url>?v=1&encoding=json` (`wss://` in production; rustls).
2. Server → `{"op": 10, "d": {"heartbeat_interval": 41250}}` (HELLO).
3. Client → IDENTIFY:

   ```json
   {
     "op": 2,
     "d": {
       "token": "tapp.<...>",
       "properties": { "os": "linux", "browser": "grapple", "device": "grapple" }
     }
   }
   ```

   (`"Bearer tapp.<...>"` also works. `intents` and `presence` are ignored for apps.)

4. Server → `READY` (op 0, `s: 1`):

   ```json
   {
     "v": 1,
     "user": { "id": "…", "username": "josh", "global_name": "Josh", "avatar": null, "...": "…" },
     "session_id": "<32 hex>",
     "resume_gateway_url": "wss://<host>/gateway",
     "application": { "id": "grapple", "name": "Grapple", "description": "…" },
     "scopes": ["identify", "presence.game", "friends.read"],
     "guilds": [],
     "relationships": [{ "id": "<friend id>", "type": 1, "user": { "...": "…" }, "nickname": null, "since": "…" }],
     "presences": [{ "user": { "id": "<friend id>" }, "status": "online", "activities": [ … ] }]
   }
   ```

   `relationships` and `presences` are empty without `friends.read`. `presences` lists only friends
   who are not offline.

5. Heartbeat every `heartbeat_interval` ms (first one after a random fraction of it, as Discord):
   `{"op": 1, "d": <last seq or null>}` → `{"op": 11}`. No heartbeat for `1.5 × interval + 5 s` closes
   the socket (`4009`). If no ACK arrives before the next heartbeat is due, treat the connection as
   dead and reconnect.

### Dispatches an app session receives

Only these (with `friends.read`), plus the game invite events of section 7 (with `invites`);
nothing else ever arrives (no messages, DMs, servers, friend requests or blocks):

- `PRESENCE_UPDATE` (a friend's presence changed; never has `guild_id`):

  ```json
  {
    "user": { "id": "<friend id>" },
    "status": "online" | "idle" | "dnd" | "offline",
    "activities": [
      { "name": "Custom Status", "type": 4, "state": "brb", "emoji": null },
      {
        "name": "Hades II",
        "type": 0,
        "created_at": 1760780000000,
        "timestamps": { "start": 1760779000000 },
        "assets": { "large_image": "steam:1145350" }
      }
    ]
  }
  ```

  The game is the activity with `type: 0` (at most one); "playing since" is `timestamps.start`. No
  `type: 0` means not playing. `offline` comes with `activities: []`. The same friend's update replaces
  the previous one. `large_image` is `steam:<appid>` or `icon:<sha256>`; fetch the picture with the
  routes in section 3.
- `RELATIONSHIP_ADD` `{id, type: 1, user, nickname: null, since}`: a new friend.
- `RELATIONSHIP_REMOVE` `{id, type: 1, nickname: null}`: no longer friends (drop their presence).
- `RESUMED` after a resume.

### Sending the game (op 3, needs `presence.game`)

```json
{
  "op": 3,
  "d": {
    "activities": [
      {
        "name": "Hades II",
        "type": 0,
        "timestamps": { "start": 1760779000000 },
        "assets": { "large_image": "steam:1145350" }
      }
    ]
  }
}
```

Clear it with `{"op": 3, "d": {"activities": []}}`.

- `name` 1–128 characters. `timestamps.start` = when the game started, in ms; not in the future (60 s
  of clock difference is forgiven), at most 7 days ago. `assets` optional;
  `large_image` = `steam:<digits>` or `icon:<64 lowercase hex>`.
- Exactly one activity of type 0. Anything invalid counts as "no game" (the game is cleared), the
  socket stays open.
- Sending the same game again changes nothing (no traffic to friends); re-send the current game after
  every new IDENTIFY (a new session starts with no game), not after a RESUME.
- `status`, `afk` and `since` are ignored for apps.
- Effects: while Grapple reports a game, Josh shows as online (unless he chose invisible, then
  nobody sees anything) and **Grapple's game wins over the tether desktop app's own detected game**.
  When Grapple clears it, the desktop's detected game (if any) shows again; if no tether window is
  open, Josh goes offline. Grapple connected without a game never makes Josh look online.

### Resume and reconnect

- On a dropped socket (not a clean close), reconnect to `resume_gateway_url` and send
  `{"op": 6, "d": {"token": "<current access token>", "session_id": "…", "seq": <last s>}}` within 2
  minutes. Success replays missed dispatches then `RESUMED`. Use the current access token (after a
  refresh, the new one; it is the same sign-in).
- `{"op": 9, "d": false}` (INVALID_SESSION): wait 1–5 s, IDENTIFY again from scratch (and re-send the
  game).
- `{"op": 7}` (RECONNECT; tether does not send it today): close and resume.
- Back off between failed connects: 1 s, 2 s, 4 s … up to 60 s, with jitter; reset after a READY.
- Closing with code 1000/1001 ends the session (no resume); Grapple does that on sign-out and quit.

### Close codes

| Code | Meaning                                           | Grapple does                                              |
| ---- | ------------------------------------------------- | --------------------------------------------------------- |
| 4000 | unknown error                                     | reconnect (resume)                                        |
| 4002 | decode error (bad frame)                          | reconnect, IDENTIFY fresh (a bug)                         |
| 4003 | not authenticated (frame before IDENTIFY)         | reconnect, IDENTIFY fresh                                 |
| 4004 | authentication failed / sign-in revoked           | refresh once and IDENTIFY; if the refresh fails, signed out |
| 4005 | already authenticated                             | reconnect, IDENTIFY fresh (a bug)                         |
| 4008 | rate limited                                      | reconnect after backoff                                   |
| 4009 | session timed out (no heartbeat)                  | reconnect, IDENTIFY fresh                                 |
| 4100 | the account must confirm its email first          | stop; tell the person                                     |
| 4101 | not allowed for this app (op 3 without `presence.game`, op 4, op 8) | stop sending that; reconnect without it |

## 5. Summary of limits

| What                         | Limit                               |
| ---------------------------- | ----------------------------------- |
| Code                         | 2 minutes, once                     |
| Access token                 | 7 days (`expires_in` 604800)        |
| Refresh token                | one use; lapses after 90 days unused |
| Sign-ins per person for Grapple | 5 (one per computer); a sixth drops the oldest |
| Token endpoint               | 30 / 5 min per address              |
| Icon uploads                 | 30 / hour per person, PNG ≤ 64 KB   |
| Gateway frames               | 120 / min, 16 KB each               |
| Resume window                | 2 minutes                           |
| Game invites sent            | 10 / 10 min per sender; 3 / 10 min to the same friend; 20 / hour to one person (section 7) |
| Game invite reads and answers | 120 / min per person               |
| Game invite open for         | 15 min by default, 60 s – 1 hour    |

## 6. Testing

- `crates/grapple-tether/tests/tether.rs` runs everything above against an in-memory fake tether
  (`tests/common/mod.rs`); the browser is stubbed (no window ever opens), the keyring is a
  `MemoryStore`.
- `crates/grapple-tether/tests/dev_lite.rs` (ignored by default) runs the real flow against a local
  `pnpm dev:lite --seed` of tether's `main` (or `grapple-invites` for section 7), with the seeded accounts:
  `GRAPPLE_TETHER_DEV_URL=http://localhost:<web port> GRAPPLE_TETHER_DEV_PASSWORD=password nice -n 19
  cargo test -p grapple-tether --test dev_lite -- --ignored`. It refuses any non-local address.

## 7. Invites ("come play with me")

Grapple invites a tether friend to a game. tether posts the invite into the 1:1 DM between the two
people **as the sender**, marked "via grapple", and tether's apps draw it as a game card with **Play**
(and **Join voice** when the sender is in a voice channel the friend may join) and **Decline**. Play
in tether reaches the friend's Grapple over the gateway, or, when no Grapple of theirs is connected,
through the deep link `grapple://invite/<id>`. Grapple then decides what Play means on that computer
(launch, install, or the store page).

tether side: `packages/shared/src/game-invites.ts` (shapes, limits, `GAME_INVITE_LINK_RE`),
`apps/server/src/services/game-invites.ts`, `docs/apps.md` "grapple: invites".

### 7.1 The scope

- Scope **`invites`** (Grapple only). Ask for `identify presence.game friends.read invites` (section
  2). The page then asks "Allow grapple to show what you play, see what your friends play and invite
  your friends to play?" and lists "Send game invites to your friends in your DMs, as you."
- A sign-in made before has no `invites`: every route below answers it `403`
  `{"code": 50001, "message": "Missing Access"}`, and its gateway session gets none of the events.
  READY's `scopes` (and the token response's `scope`) say whether the sign-in has it. Without it,
  Grapple shows **Allow invites**, which runs the sign-in of section 2 again with the four scopes
  (then stores the new pair and revokes the old refresh token, as in "Signing in again").
- All routes take `Authorization: Bearer <access token>` (the app token). Bodies are JSON
  (`Content-Type: application/json`); the action routes (`.../seen`, `/accept`, `/decline`, `/cancel`)
  take **no body** (send none, and no `Content-Type`).

### 7.2 The invite object (`APIGameInvite`)

```json
{
  "id": "1575162397511909376",
  "channel_id": "1575162397511909100",
  "message_id": "1575162397511909376",
  "sender_id": "1575160000000000001",
  "recipient_id": "1575160000000000002",
  "game": { "id": "steam:252950", "name": "Rocket League", "image": "steam:252950" },
  "state": "sent",
  "joinable": false,
  "created_at": "2026-11-25T18:00:00.000Z",
  "expires_at": "2026-11-25T18:15:00.000Z",
  "seen_at": null,
  "answered_at": null
}
```

- `id`: the invite's id (a snowflake; today equal to `message_id`, but don't rely on that). It is the
  `<id>` of the deep link.
- `channel_id` / `message_id`: the 1:1 DM and the message that shows the card.
- `game.id`: exactly what Grapple sent. `game.name`: tether's cleaned copy (one line; `<` `>` `[` `]`
  `{` `}` replaced by look-alikes, `://` broken): match games by `game.id`, and by name only as a
  fallback for `manual:` ids (they differ per computer). `game.image`: `steam:<appid>`,
  `icon:<sha256>` or `null`; fetch the picture with the routes of section 3.
- `state`, one of:

  | `state`     | Meaning                                                                  | Sender's Grapple shows |
  | ----------- | ------------------------------------------------------------------------ | ---------------------- |
  | `sent`      | posted, not yet shown to the friend                                      | Sent / "No reply · 9 min" |
  | `seen`      | the friend's tether (or Grapple) showed it                               | Seen                   |
  | `accepted`  | the friend pressed Play (in tether or Grapple); their Grapple was told   | Joined                 |
  | `declined`  | the friend said no                                                       | Declined               |
  | `cancelled` | the sender took it back                                                  | Cancelled              |
  | `expired`   | `sent` or `seen` past `expires_at`                                       | Expired                |

  An open invite (`sent`/`seen`) past `expires_at` is answered as `expired` at once, even before
  tether's minute sweep stores it (the sweep then sends `GAME_INVITE_UPDATE`). `accepted` stays
  `accepted` after `expires_at`. `declined`, `cancelled` and `expired` never change again. Grapple
  should also treat an open invite past `expires_at` as expired on its own clock.
- `joinable`: a `join` payload came with it (the payload itself is never in this object).
- `seen_at`, `answered_at` (accepted/declined/cancelled): ISO 8601 or `null`.

### 7.3 Sending: `POST /api/v1/users/@me/game-invites`

```json
{
  "recipient_id": "1575160000000000002",
  "game": { "id": "steam:252950", "name": "Rocket League", "image": "steam:252950" },
  "expires_in": 900,
  "join": { "kind": "text", "value": "Private match: grapple / hunter2" }
}
```

- Only an app token with `invites` (a person's own session: `403 50001`).
- No other field anywhere (an unknown field is `400 50035`).
- `recipient_id`: a friend's id (from `GET /users/@me/relationships` or READY's `relationships`).
- `game.id` must match `^(steam:\d{1,10}|epic:[A-Za-z0-9._:-]{1,128}|manual:[A-Za-z0-9-]{1,64})$`.
- `game.name`: 1–128 characters once trimmed.
- `game.image` (optional): `steam:<1-10 digits>` or `icon:<64 lowercase hex>` (upload the PNG first,
  section 3). Left out for a `steam:` game, tether uses `steam:<appid>`; for others the card draws a
  coloured tile with a controller.
- `expires_in` (optional): whole seconds, **60 to 3600**; default **900** (15 minutes).
- `join` (optional, v1): `{"kind": "text", "value": <1–256 characters after trimming, no control
  characters>}`, e.g. a private match's name and password. See 7.8. Any other `kind` is `400 50035`
  in v1.

`201` → the `APIGameInvite` (`state: "sent"`). At the same moment both people's Grapple sessions get
`GAME_INVITE_CREATE`, and the friend's tether shows the card (and a push notification, "Game invite:
Rocket League" in their language, unless the DM is muted).

Errors (`{"code", "message"}`; `50035` adds `"errors"` by field):

| Status | `code`  | When                                                                                   |
| ------ | ------- | -------------------------------------------------------------------------------------- |
| 400    | `50035` | a field is wrong or extra                                                              |
| 401    | `40001` | no token, a bad, expired or revoked token (refresh once, then sign in again)            |
| 403    | `50001` | the sign-in has no `invites`                                                           |
| 403    | `50007` | not allowed to invite that person: not friends (both ways), a block either way, their DM settings, their "Who can invite you to play" is Nobody, a bot, system or deleted account. Never says which; show "Can't invite <name>". |
| 404    | `10013` | no such user, or yourself                                                              |
| 429    | `90003` | a limit (7.9); body has `retry_after` (seconds), header `Retry-After`. A refused try counts. |

### 7.4 Reading

- `GET /api/v1/users/@me/game-invites?limit=<1-100, default 50>` → `APIGameInvite[]`: the invites
  this person **sent or received** in the last 24 hours, newest first (Grapple's "Invites" list,
  mockup screen D: filter by `sender_id == me`). A deleted message hides its invite.
- `GET /api/v1/users/@me/game-invites/<id>` → the `APIGameInvite`, for the sender or the recipient
  only; asked with the **recipient's app token** it also has `"join": {...}` when a payload exists,
  the state is `sent`, `seen` or `accepted`, and `expires_at` has not passed. `404`
  `{"code": 90270, "message": "Unknown Game Invite"}` for anyone else, a wrong id or a deleted
  message.

### 7.5 Answering

All four answer `200` with the `APIGameInvite` (accept: `{invite, app_notified}`), take no body, and
`404 90270` for someone who is neither sender nor recipient.

| Route                                  | Who            | Effect                                                         |
| -------------------------------------- | -------------- | -------------------------------------------------------------- |
| `POST .../game-invites/<id>/seen`      | recipient      | `sent` → `seen`. Otherwise no change (still `200`).            |
| `POST .../game-invites/<id>/accept`    | recipient      | `sent`/`seen` → `accepted`; `accepted` before `expires_at`: no change (Play again). |
| `POST .../game-invites/<id>/decline`   | recipient      | `sent`/`seen` → `declined`; already `declined`: no change.     |
| `POST .../game-invites/<id>/cancel`    | sender         | `sent`/`seen` → `cancelled`; already `cancelled`: no change.   |

- The wrong one of the two (the sender accepting, the recipient cancelling): `403 50001`.
- Anything else on an invite that ended (accept after `expires_at`, decline after accept, cancel
  after accept, accept after decline/cancel): `400 {"code": 90271, "message": "This game invite is no
  longer open."}`.
- `accept` → `{"invite": APIGameInvite, "app_notified": bool}`. Asked with an **app token** (Play in
  Grapple's own toast or invite page) `app_notified` is always `false` and **no** `GAME_INVITE_PLAY`
  is sent: Grapple acts itself. (Asked by tether with the person's session, the server sends
  `GAME_INVITE_PLAY` to the recipient's Grapple sessions and `app_notified` says whether one existed.)
- Grapple marks an invite `seen` when it shows its own toast or invite page for it (the recipient's
  tether does the same when the card is on screen).
- Every change sends `GAME_INVITE_UPDATE` to both people's Grapple sessions and redraws both cards.

### 7.6 Gateway events (app sessions holding `invites` only)

Ordinary dispatches (`op: 0`, with `s`, replayed on RESUME). They go only to app sessions whose
sign-in has `invites`: never to the person's tether windows, to bots, or to a Grapple sign-in
without the scope. Unknown future fields must be ignored.

- `GAME_INVITE_CREATE` `{"invite": APIGameInvite}`: an invite was sent **by** this person (from any of
  their computers) **or to** them. Recipient's Grapple: show a toast ("Josh · Rocket League", **Play**
  / ✕) until `expires_at`, and call `.../seen` when shown. Sender's Grapple: add it to the Invites list.
- `GAME_INVITE_UPDATE` `{"invite": APIGameInvite}`: the state changed (seen, accepted, declined,
  cancelled, expired). Replace the stored copy by `invite.id`; drop the toast when it is no longer open.
  The sender's "Lerato joined" toast comes from `state: "accepted"`.
- `GAME_INVITE_PLAY` `{"invite": APIGameInvite, "join": {"kind": "text", "value": "..."}}` (`join`
  only when there is a payload): **the person pressed Play on the card in tether** on some computer.
  Only to that person's own Grapple sessions (the recipient's). Grapple acts at once, with no
  question: launch the game if installed, else start the install if owned, else open its store page;
  with a `text` payload, copy it to the clipboard and say so. `invite.state` is `accepted`. Do not
  call `accept` again. If several computers of the person run Grapple, each gets it; act only if
  this computer has a reason to (e.g. the game is installed here, or this is the only one) — a v1
  choice for Grapple.

### 7.7 The deep link `grapple://invite/<id>`

- Format exactly `grapple://invite/<1–20 decimal digits>` (regex
  `^grapple:\/\/invite\/(\d{1,20})$`); no query, fragment, trailing slash or other path. Reject
  anything else without acting.
- tether opens it only when Play's `accept` answered `app_notified: false` (no Grapple of the
  person's connected). In a browser, the browser asks "Open grapple?"; tether's desktop app lets
  exactly this shape through to the system and nothing else of `grapple:`.
- The link carries only the id. Grapple then: starts (single instance: hand the URL to the running
  one), signs in if needed, `GET /users/@me/game-invites/<id>` with its app token (this also gets the
  `join` payload), and shows the invite with **one** Play button. A deep link never starts a game by
  itself (anything on the web can open such a link); only the gateway's `GAME_INVITE_PLAY` acts
  directly. On Play, Grapple calls `accept` with its app token (`200` also when tether already
  accepted it, until `expires_at`), then acts. `404 90270`: "This invite isn't for this account" (the
  link was for another tether account, or the message was deleted); `state` not open (or `accepted`
  past `expires_at`): show the state (Expired / Declined / Cancelled), no Play.

### 7.8 The `join` payload (privacy)

- v1 accepts only `{"kind": "text", "value": ...}`. v2 will add Steam kinds (`steam_lobby`,
  `steam_connect`) with their own fields inside the same object; Grapple must ignore kinds it does
  not know. Until a tether has v2, sending them is `400 50035` (send again without `join`).
- tether never stores it (not in the database, the message, storage or a log): it is kept in the
  server's memory until the invite is declined, cancelled or reaches `expires_at`, and a server
  restart loses it (`joinable` stays `true`, `join` is then simply absent: treat it as "no payload").
- It goes only to the recipient's Grapple: in `GAME_INVITE_PLAY` and in `GET .../<id>` with the
  recipient's app token. The sender's reads, everyone's tether windows and the message never see it.
- Grapple must not log it either, and uses it only as data (clipboard, or one argument inside its
  own launch template; never to choose a program).

### 7.9 Limits and settings

- Sending: 10 per 10 minutes per sender, 3 per 10 minutes from one person to the same friend, 20 an
  hour to one person from everyone. Reads and answers (all the other routes of this section): 120 a
  minute per person. `429` as in 7.3.
- Open for `expires_in` seconds: 60 to 3600, default 900.
- **"Who can invite you to play"** (mockup screen F, enforced by tether): `GET
  /api/v1/users/@me/game-invite-settings` → `{"from": "friends" | "nobody"}`; `PUT` the same object
  (`200` echoes it; anything else `400 50035`). The same switch is in tether's Settings › Privacy.
  There is no separate "never from" list in tether: blocking a person in tether stops their invites
  (`403 50007` to the sender). Grapple's "Never from" should open tether's block, or be Grapple-only
  (hide their toasts).
- **Not in v1:** the card cannot show Install / Get (tether doesn't know what is installed or owned;
  it always says Play; Grapple reporting that state back is a follow-up), group invites, or inviting
  a voice channel. One invite per friend: send several for several friends.

### 7.10 Testing

Against `pnpm dev:lite --seed` of the `grapple-invites` branch (two seeded accounts that are
friends, each signed in to Grapple with `invites`): send, the toast on the other side, Play in
tether with Grapple running (gateway) and with Grapple closed (deep link), decline, cancel, expire
(`expires_in: 60`). The fake tether in `crates/grapple-tether/tests/common/mod.rs` needs the routes
and events above.
