# Epic prices with Josh's Epic login

Question (docs/STORE_PLAN.md, "Epic catalog/prices"): can the Epic login grapple-launch already refreshes read
catalog offer prices? Researched from source code and captured traffic only. **No Epic endpoint was called** with
Josh's tokens (or anonymously) while building this.

## Answer

Very likely yes. The token in legendary's `user.json` belongs to the Epic Games Launcher client
(`34a02cf8f4414e29b15921876da36f9a`, an `eg1` token). EpicResearch's permission list for that client
(`MixV2/EpicResearch` `docs/auth/permissions/34a02cf8f4414e29b15921876da36f9a.md`) includes:

- `catalog:shared:*` Read (lines 11, 55),
- `priceengine:shared:offer:price` Read (line 67),
- `priceengine:shared:salesevent` Read (lines 16, 99),
- `payment:public:{accountId}:billingaccount` Read.

None of the routes below need a store cookie; all take `Authorization: bearer <access_token>`. Get the token with
`grapple_launch::epic_auth::EpicAuth::session()`: it refreshes under legendary's lock and writes the rotated tokens
back to `user.json` (Epic rotates refresh tokens; anything else would log Heroic out).

Not yet verified against Epic: that priceengine accepts this exact token (the permission is listed, but no
open-source launcher calls it with checked parsing), batch limits, and the meaning of two fields (below).

## Routes

### 1. Price engine (preferred: exact current price, bulk)

`POST https://priceengine-public-service-ecomprod01.ol.epicgames.com/priceengine/api/shared/offers/price`,
`Content-Type: application/json`.

```json
{ "accountId": "<account_id>", "calculateTax": false, "country": "ZA",
  "lineOffers": [ { "offerId": "<offerId>", "quantity": 1 } ] }
```

Response (from captured traffic):

```json
{ "country": "ZA", "namespace": "<ns>",
  "totalPrice": { "currencyCode": "USD", "...": "..." },
  "totalPaymentPrice": { "paymentCurrencyCode": "USD", "paymentCurrencySymbol": "$",
                         "paymentCurrencyAmount": 1999, "paymentCurrencyDecimal": 2 },
  "lineOffers": [ { "offerId": "<offerId>", "ref": "...", "appliedRules": [],
      "price": { "currencyCode": "USD", "originalPrice": 1999, "originalUnitPrice": 1999,
                 "discountPrice": 999, "unitPrice": 999, "discount": 1000, "voucherDiscount": 0,
                 "discountPercentage": 50, "vat": 0, "vatRate": 0,
                 "basePayoutPrice": 1999, "basePayoutCurrencyCode": "USD" } } ] }
```

- Amounts are integers in the currency's minor unit; decimals come from `paymentCurrencyDecimal`.
- Compute the discount as `1 - discountPrice / originalPrice`. Do not use `discountPercentage`: the capture shows
  `100` with no discount, so it seems to be "percent of the original you pay".
- Sale end: probably `lineOffers[].appliedRules[].endDate`. The captured sample had no rules; the field name comes
  from the GraphQL shape in route 3.
- One namespace per call is the safe assumption (the response carries one `namespace`). The capture sent 10 offers.

Sources:
- `AchetaGames/Epic-Asset-Manager` `docs/api-map.md` lines 704-710. This is launcher traffic captured with
  mitmproxy on 2026-02-16, described as a "bulk price lookup".
- `Breakers-Revived/battle-breakers-documentation`
  `docs/Price Engine Service/priceengine/api/shared/offers/price.md`: the request is at lines 30-77 and the
  response at lines 99-160.
- `SzymonLisowiec/node-epicgames-client` `resources/Endpoint.js` line 49.
- `AchetaGames/egs-api-rs` `src/api/commerce.rs` sends a different body (`{namespace, offers, country}`) that
  neither capture matches. Treat its shape as unverified.

### 2. Catalog bulk offers (fallback: list price and metadata)

`GET https://catalog-public-service-prod06.ol.epicgames.com/catalog/api/shared/bulk/offers?id=<o1>&id=<o2>&returnItemDetails=false&country=ZA&locale=en`

Legendary already calls this host with the same session for items (`legendary/api/egs.py` lines 31 and 211-217).

The response is an object keyed by offer id. Each value is the full offer:
`{ title, namespace, currencyCode, currentPrice, price, basePrice, basePriceCurrencyCode, currencyDecimals,
priceTier, vatIncluded, effectiveDate, expiryDate, ... }`, with amounts in minor units.

It is unknown whether `currentPrice` reflects a running sale. The sample had none. There is no sale end date.

Sources:
- battle-breakers-documentation `docs/Catalog Service/catalog/api/shared/bulk/offers.md` lines 1-44 and 66-140.
- `jackmoore7/fortnite-bot` `imports/api/api_epic.py` lines 45-60.

### 3. Launcher store GraphQL (what Rare's store tab uses)

`POST https://launcher.store.epicgames.com/graphql`: note the `launcher.` host, not `store.epicgames.com`, which
is the one that answers 403 from outside. Send an optional bearer token and the launcher user agent
(`EpicGamesLauncher/14.0.8-22004686+++Portal+Release-Live`).

`Catalog.catalogOffer(namespace, id, locale) { price(country) { totalPrice { discountPrice originalPrice
voucherDiscount discount currencyCode currencyInfo { decimals } fmtPrice(locale) { originalPrice discountPrice } }
lineOffers { appliedRules { id endDate discountSetting { discountType } } } } }` returns one offer per call.
`searchStore(namespace: ..., keywords: ...)` returns many offers per call, with the same price block.

Sources: Rare
- `rare/components/tabs/store/__init__.py` lines 17-23,
- `rare/components/tabs/store/store_api.py` lines 39-42 and 100-118,
- `rare/components/tabs/store/api/constants/queries.py` lines 364-431,
- `rare/shared/rare_core.py` line 177.

It is an inference that this host is not behind the Cloudflare block: Heroic, Rare and legendary all use it in
production.

### Josh's currency

Every route takes `country` and answers in that country's currency. What Epic actually charges him comes from
`GET https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/payment/accounts/{accountId}/billingaccounts/default`
→ `{ billingAccountName, country, countrySource, currency }` (egs-api-rs `src/api/commerce.rs` lines 51-59).
Ask once and cache it. That is the "Epic from his Epic account" currency detection in STORE_PLAN.

## Why there is no `EpicPriceSource` impl yet

`grapple_store::epic::EpicPriceSource::quotes(country, steam_appids)` is keyed by **Steam app id**. That fits
IsThereAnyDeal, but Epic's services only know Epic ids. The only bridge on Epic's side is a title search
(`searchStore(keywords)`), which is fuzzy, and the catalog bulk offers response has no Steam id to check a match
against. An impl on that trait would have to guess, and STORE_PLAN says Epic prices are never invented.

A clean fit needs an offer-keyed entry point next to the existing one, for the games where grapple already has
the Epic ids (Epic-only wishlist entries, `epic:<ns>:<offer>` keys from the free-games feed or legendary
metadata):

```rust
/// Prices of Epic offers by their ids. Ids missing from the map were not answered.
fn offer_prices<'a>(&'a self, country: &'a str, offers: &'a [(String /*ns*/, String /*offerId*/)])
    -> BoxFuture<'a, Result<HashMap<(String, String), Option<EpicQuote>>>>;
```

The impl (priceengine first, catalog bulk offers if priceengine refuses) is about 100 lines on top of
`EpicAuth::session()`, tested against a mock server. Before it ships, Josh should make the first real call
deliberately, with one known offer, and compare the result with the store page.

Cache the results with the store's 6 h price TTL. A 401 means refresh once through `EpicAuth` (it handles the
rotation), then report "unavailable", never a guess.
