//! READ-ONLY probe of the Steam bridge, for Josh's machine:
//!
//!     cargo run -p grapple-download --example steam_probe [-- <port>]
//!
//! Prints whether Steam's DevTools endpoint is there (it needs "Steam download control" on, or the empty file
//! `~/.steam/steam/.cef-enable-remote-debugging` and a Steam restart), which `SteamClient.Downloads`, `.Installs`
//! and `.InstallFolder` functions this Steam has, the library folders, and the current download items.
//! It never calls an action (no pause, install, queue or uninstall): only `Object.keys`, `GetInstallFolders` and the
//! two download-list subscriptions. It never prints the websocket URL.

use grapple_download::steam_bridge::{self, BridgeEvent, Call, Status, SteamBridge, discover, parse_folders};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(steam_bridge::DEFAULT_PORT);
    let http = reqwest::Client::builder().no_proxy().build().expect("http client");
    match discover(&http, port).await {
        Ok(_) => println!("bridge: Steam's SharedJSContext found on 127.0.0.1:{port}"),
        Err(e) => {
            println!("bridge: unavailable on 127.0.0.1:{port}: {e:?}");
            println!("(turn on \"Steam download control\" in grapple, or create the empty file .cef-enable-remote-debugging in the Steam folder and restart Steam)");
            std::process::exit(1);
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let b = SteamBridge::spawn(port, tx);
    let mut watch = b.status_watch();
    let connected = tokio::time::timeout(Duration::from_secs(10), async {
        while *watch.borrow() != Status::Connected {
            if watch.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    if connected.is_err() || b.status() != Status::Connected {
        println!("bridge: could not connect");
        std::process::exit(1);
    }
    for (ns, names) in b.features() {
        println!("\nSteamClient.{} ({} functions):", ns.name(), names.len());
        for n in names {
            println!("  {n}");
        }
    }
    println!("\nfunctions grapple uses:");
    for c in [
        Call::EnableAllDownloads(true),
        Call::PauseAppUpdate(0),
        Call::ResumeAppUpdate(0),
        Call::QueueAppUpdate(0),
        Call::SetQueueIndex(0, 0),
        Call::RemoveFromDownloadList(0),
        Call::RegisterDownloadItems,
        Call::RegisterDownloadOverview,
        Call::SetAppList(0),
        Call::SetInstallFolder(0),
        Call::ContinueInstall,
        Call::GetInstallManagerInfo,
        Call::OpenInstallWizard(0),
        Call::ContinueWizardOnce(0),
        Call::OpenUninstallWizard(0),
        Call::GetInstallFolders,
    ] {
        let (ns, f) = c.needs().expect("needs a function");
        println!("  {:<40} {}", format!("{}.{f}", ns.name()), if b.has(c) { "yes" } else { "MISSING" });
    }
    if b.has(Call::GetInstallFolders) {
        match b.call(Call::GetInstallFolders).await {
            Ok(v) => {
                println!("\nlibrary folders (GetInstallFolders):");
                for f in parse_folders(&v) {
                    println!("  index {} default {} path {}", f.index, f.is_default, f.path.as_deref().unwrap_or("(not given)"));
                }
                println!("  raw keys of the first entry: {:?}", v.get(0).and_then(|x| x.as_object()).map(|o| o.keys().cloned().collect::<Vec<_>>()));
            }
            Err(e) => println!("GetInstallFolders: {e}"),
        }
    }
    println!("\nwaiting 3 s for the download list…");
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    while let Ok(Some(ev)) = tokio::time::timeout_at(until, rx.recv()).await {
        match ev {
            BridgeEvent::Items { downloading, items } => {
                println!("items (downloading: {downloading}):");
                for i in items {
                    println!(
                        "  app {} active {} paused {} completed {} queue {} {}/{} bytes build {}→{} {}",
                        i.appid, i.active, i.paused, i.completed, i.queue_index, i.downloaded, i.total, i.buildid, i.target_buildid, i.update_error
                    );
                }
            }
            BridgeEvent::Overview(o) => println!(
                "overview: app {} state {} paused {} {}/{} bytes {} B/s {} s left install {}",
                o.appid, o.state, o.paused, o.downloaded, o.to_download, o.bytes_per_sec, o.seconds_left, o.is_install
            ),
            _ => {}
        }
    }
    b.stop();
}
