# grapple store: build plan

Approved design: `mockups/06-store.html` (store front, Home section, game page, settings) and the wishlist page
from `mockups/05-store.html` (screen D), reached from the store bar's "♥ Wishlist" pill (there is no Wishlist
tab in the top bar). Build what they show. The generator for 06 is `mockups/assets/store/gen_store2.py`.

grapple builds its **own** store pages from Steam's and Epic's data. It never embeds their store pages, except
the real checkout (and sign-in) pages, which open in grapple's isolated store window when Josh presses Buy.

## Rules from Josh (all binding)

- Copy is terse: section titles alone or with a 2-4 word subtitle ("Top games · This week's best-selling games",
  "Deals · On sale now", "Free on Epic", "Browse", "Nothing on your wishlist is on sale right now."). No
  explanatory sentences in the UI.
- Green text/badges only for a price advantage (discount badge, "Cheaper on Steam/Epic"). Reasons ("similar to")
  are shown as small stacked icons of Josh's own games plus one short word ("Like"), never playtime.
- Art is always the store's official art (capsule, header, library hero, logo), never in-game screenshots
  (screenshots only in the game page's screenshot strip).
- The featured hero at the top of the store nearly fills the window (see 06: ~735 px tall in a 800 px window).
- Rows vary in layout (hero carousel, two big tiles + small cards, numbered top-10 in two columns, wide deal
  cards, dense price-tier grid, cover row, compact wishlist strip, Epic free cards, genre tiles).
- Display currency setting: every price grapple shows is converted into the one currency Josh picks (default:
  the Steam store currency). Each store's native price is kept for checkout. Rates: one daily fetch,
  cached (`https://open.er-api.com/v6/latest/USD`, free, no key), shown nowhere except settings.
- "Hide adult games" (default on): Steam content descriptors 1, 3, 4 **or** community tags Sexual Content,
  Nudity, NSFW, Hentai (descriptors alone miss games: "Five Nights with Maids" has none but is tagged).
  "Hide very violent games" (default off): descriptor 2. Hidden counts appear per row ("1 hidden by your filter · Show").
- Never show games Josh owns (Steam library from grapple-scan, Epic from legendary metadata).
- Wishlist: Josh's public Steam wishlist (`IWishlistService/GetWishlist?steamid=…`) plus grapple's own entries
  (needed for Epic games). Sale badges on either store; a per-game "Tell me on sale" switch; a desktop
  notification when a wishlisted game goes on sale.
- Detect the Steam country from Steam's own files if possible (else a setting); Epic from the Epic account.

## Data sources (public, no login unless stated)

| What | Source |
| --- | --- |
| Steam lists | `store.steampowered.com/search/results/?json=1&cc=<cc>&category1=998` with `filter=topsellers`, `filter=popularnew&sort_by=Released_DESC`, `maxprice=<n>`; `api/featuredcategories` (specials); `ISteamChartsService/GetMostPlayedGames/v1` |
| Steam game details + tags | `IStoreBrowseService/GetItems/v1` (keyless, 50 apps per call: basic info, community tag ids with weights, content descriptors, release, reviews, platforms, screenshots; tag names from `IStoreService/GetTagList/v1`, cached a week). Fallbacks: `api/appdetails?appids=<id>&cc=<cc>&l=english` (one id per call) for apps GetItems does not describe; `filters=price_overview` accepts many ids (bulk prices) |
| Steam community tags | from `GetItems` (above); only for tag ids the tag list cannot name, or apps GetItems does not describe: the game's store page HTML (`InitAppTagModal` JSON: `"name":"…","count"`), with the age-gate cookies `birthtime`, `mature_content=1` |
| Steam art | `shared.akamai.steamstatic.com/store_item_assets/steam/apps/<id>/{library_600x900,library_hero,header,logo}.jpg/png` |
| Epic free games | `store-site-backend-static.ak.epicgames.com/freeGamesPromotions?country=<cc>` |
| Epic catalog/prices | **unknown**: the public store GraphQL returns 403 from outside. Investigate Epic's authenticated services with Josh's Epic login (the one grapple-launch already refreshes), else IsThereAnyDeal's API (free key, Josh provides it in settings). Until solved, Epic prices show as unavailable, never invented. |

Rate limits: Steam store ≈200 requests / 5 min. Everything is cached in SQLite with TTLs (lists 1 h, details
24 h, prices 6 h, tags 7 days, art on disk forever) and refreshed in the background; the UI always renders from
cache first. Every price fetch is also written to a `price_history` table so "lowest grapple has seen" works.

## Recommendations ("Picked for you", "Games we think you'd like")

Explainable scoring in Rust:
- taste profile: Josh's playtime per genre and community tag across his library (log-scaled, recent play weighted up);
- purchase pattern: genres/tags of games added to his library recently (best signal available locally);
- friends: tether friends who play or own it;
- price fit: discount and price against his usual spend; wishlist boost;
- exclusions: owned, filtered (adult/violent), not released.
Each pick carries `similar_to`: the 2-3 owned games sharing the most weighted tags (for the icon stack), or
friends. "Games we think you'd like" on Home = the top 5 by score with price weighted a little higher.

## Checkout (security-critical)

- A separate Tauri window ("store") with its **own persistent website data directory** (cookies persist, so
  Josh signs in once). It has **no** Tauri IPC/capabilities at all (no commands, no events): it is a plain browser
  for store.steampowered.com, checkout.steampowered.com, login.steampowered.com, steamcommunity.com (login only),
  store.epicgames.com, www.epicgames.com. Navigation outside an allow-list opens in the system browser instead.
- Steam: open the game's purchase straight away where possible (inside the store window, the same-origin "add to
  cart" then cart/checkout), else the game's page at its purchase box. Josh signs in himself on Steam's page
  (password or QR); grapple never types or stores credentials, only the window's cookies persist.
- Epic: sign the store window in automatically with an exchange code from Josh's Epic login
  (`https://www.epicgames.com/id/exchange?exchangeCode=<code>&redirectUrl=<checkout url>`), then
  `https://store.epicgames.com/purchase?offers=1-<namespace>-<offerId>`. The code is a secret: never logged, only
  ever put into that navigation.
- grapple never sees card details; payment happens on the store's own page.

## Who builds what

| Task | Owner | Writes to |
| --- | --- | --- |
| Plan, contract, review | lead | this file |
| Data, caching, recommendations, wishlist, prices, filters, FX, store commands | Sonnet | `crates/grapple-store`, `app/src-tauri/src/store*.rs`, its commands in `commands.rs`, `grapple://store/...` art route |
| Store UI (store front, game page, wishlist page, Home section, store settings) | Sonnet | `app/ui/src/components/store/*`, `app/ui/src/lib/store*.ts`, styles |
| Store window (checkout), Epic sign-in for it, Epic price source research | Opus | `app/src-tauri/src/store_window.rs`, `windows.rs`, capabilities, `crates/grapple-launch` (exchange code reuse) |

## Shared contract (TypeScript mirror in `app/ui/src/lib/store-types.ts`)

```ts
type StoreKind = 'steam' | 'epic'
type StoreKey = string                      // "steam:1621690" | "epic:<namespace>:<offerId>"
interface Money { minor: number; currency: string }           // display currency
interface StorePrice { store: StoreKind; original: Money; final: Money; discountPct: number;
                       native: { minor: number; currency: string }; url: string }   // url: where Buy goes
interface StoreArt { cover?: string; header?: string; hero?: string; logo?: string; shots: string[] }  // grapple://store/... URLs
interface Similar { kind: 'game' | 'friend'; id: string; icon?: string; name: string }
interface StoreGame { key: StoreKey; name: string; art: StoreArt; genres: string[]; tags: string[];
  short: string; developers: string[]; release?: string; reviews?: number; free: boolean; comingSoon: boolean;
  prices: StorePrice[]; cheapest?: StoreKind; owned: boolean; wishlisted: boolean; notifyOnSale: boolean;
  similar: Similar[]; lowestSeen?: Money }
interface StoreFront { featured: StoreGame[]; picked: StoreGame[]; top: StoreGame[]; deals: StoreGame[];
  under: { tierMinor: number; games: StoreGame[]; hidden: number }; newReleases: { games: StoreGame[]; hidden: number };
  wishlist: StoreGame[]; epicFree: { game: StoreGame; now: boolean; until?: string }[];
  genres: { name: string; art: string }[]; updatedAt: number }
interface StoreSettings { displayCurrency: string; hideAdult: boolean; hideViolent: boolean;
  stores: { steam: boolean; epic: boolean }; wishlistAlerts: boolean; itadKey?: boolean /* set or not, never the key */ }
```

Commands: `store_front()`, `store_game(key)` (+ `moreLikeThis`), `store_under(tierMinor, includeHidden)`, `store_new_releases(includeHidden)`, `store_search(q, filters)`,
`store_wishlist()`, `store_wishlist_set(key, on)` (off: a Steam-wishlist game is only hidden in grapple), `store_wishlist_hidden()`, `store_wishlist_unhide()`, `store_region_get/set`, `store_itad_key_set(key)`, `store_notify_set(key, on)`, `store_home_picks()`,
`store_settings_get/set`, `store_currencies()`, `store_checkout(key, store)` (opens the store window), event
`store-changed` (a section refreshed). Until the backend lands, the UI uses a mock built from
`mockups/assets/store/` data (behind the existing mock/dev switch).
