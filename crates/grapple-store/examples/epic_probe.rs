//! ONE deliberate real call to Epic's price services with Josh's Epic login, to compare with the store page.
//!
//!     cargo run -p grapple-store --example epic_probe -- <namespace> <offerId> [country]
//!
//! Nothing runs it automatically and the tests never do (they use a local mock). It reads legendary's `user.json`
//! (refreshing the token under legendary's lock and writing the rotated tokens back, exactly like a game launch),
//! asks for the account's billing country, then for the price of the one offer through the price engine and the
//! catalog. It prints prices and counts only, never a token. Look the ids up in the offer's store page URL or in
//! `~/.config/heroic/legendaryConfig/legendary/metadata/<app>.json` (`asset_infos.Windows.namespace`, `metadata.id`).

use futures_util::future::BoxFuture;
use grapple_launch::epic_auth::{EpicApi, EpicAuth, default_user_json};
use grapple_store::db::StoreDb;
use grapple_store::epic::EpicPriceSource;
use grapple_store::epic_account::{EpicAccount, EpicSession, EpicSessionProvider};
use grapple_store::net::Net;
use grapple_store::secrets::ApiKey;
use std::sync::Arc;

struct Login(EpicAuth);

impl Login {
    async fn current(&self) -> grapple_store::Result<EpicSession> {
        let (who, token) = self.0.session().await.map_err(|e| grapple_store::Error::Unavailable(e.to_string()))?;
        Ok(EpicSession { account_id: who.account_id, token: ApiKey::new(token.expose()) })
    }
}

impl EpicSessionProvider for Login {
    fn available(&self) -> bool {
        true
    }
    fn session(&self) -> BoxFuture<'_, grapple_store::Result<EpicSession>> {
        Box::pin(self.current())
    }
    fn refresh(&self) -> BoxFuture<'_, grapple_store::Result<EpicSession>> {
        Box::pin(async move {
            self.0.ensure_session(true).await.map_err(|e| grapple_store::Error::Unavailable(e.to_string()))?;
            self.current().await
        })
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(ns), Some(offer)) = (args.first(), args.get(1)) else {
        eprintln!("usage: epic_probe <namespace> <offerId> [country]");
        std::process::exit(2);
    };
    let auth = EpicAuth::new(EpicApi::default(), default_user_json()).expect("Epic login file");
    let acct = EpicAccount::new(Arc::new(Net::real().unwrap()), StoreDb::open_in_memory().unwrap(), Arc::new(Login(auth)));
    let country = match args.get(2) {
        Some(c) => c.clone(),
        None => match acct.billing().await {
            Ok(b) => {
                println!("billing account: country {} currency {}", b.country, b.currency);
                b.country
            }
            Err(e) => {
                println!("billing account: {e}; using US");
                "US".into()
            }
        },
    };
    match acct.offer_prices(&country, &[(ns.clone(), offer.clone())]).await {
        Ok(m) => match m.values().next() {
            Some(Some(q)) => println!("price in {country}: {} {:.2} (regular {:.2}), -{}%", q.price.currency, q.price.minor as f64 / 100.0, q.original.minor as f64 / 100.0, q.discount),
            _ => println!("Epic did not price that offer"),
        },
        Err(e) => println!("no answer: {e}"),
    }
}
