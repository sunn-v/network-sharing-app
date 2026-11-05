//! LAN Drive on macOS: the File Provider extension (`app/macos/LANDrive`, Swift) puts it in the
//! Finder sidebar and asks this app for listings and bytes over the localhost bridge
//! (`lanshare_core::bridge`). The bridge's port and token go in the shared App Group container.
//!
//! Needs a build made with APPLE_TEAM_ID set and the extension bundled (`app/macos/build.sh`);
//! a plain `cargo run` has no extension, so LAN Drive falls back to full copies there.

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use block2::RcBlock;
use lanshare_core::Node;
use objc2::{
    class, msg_send,
    rc::{Allocated, Retained},
    runtime::AnyObject,
};
use objc2_foundation::NSString;

#[link(name = "FileProvider", kind = "framework")]
unsafe extern "C" {}

const DOMAIN_ID: &str = "lan-drive";
const DOMAIN_NAME: &str = "LAN Drive";

fn group_id() -> Option<String> {
    // Team-ID-prefixed App Groups need no provisioning profile on macOS.
    option_env!("APPLE_TEAM_ID").map(|team| format!("{team}.cafe.getapps.networksharing"))
}

pub fn available() -> bool {
    let appex = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("PlugIns/LANDrive.appex")));
    group_id().is_some() && appex.is_some_and(|p| p.exists())
}

/// macOS picks the folder name under ~/Library/CloudStorage; find the one ending in "LAN Drive".
pub fn drive_root(home: &Path) -> PathBuf {
    if !available() {
        return home.join("LAN Drive"); // full-copy fallback
    }
    let cloud = home.join("Library/CloudStorage");
    std::fs::read_dir(&cloud)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(DOMAIN_NAME)))
        .unwrap_or_else(|| cloud.join(format!("Network Sharing-{DOMAIN_NAME}")))
}

pub fn start(node: Arc<Node>) -> Result<()> {
    let group = group_id().context("built without APPLE_TEAM_ID")?;
    let mut raw = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let port = node.runtime().block_on(lanshare_core::bridge::start(node.clone(), token.clone()))?;

    let home = std::env::var_os("HOME").map(PathBuf::from).context("no HOME")?;
    let dir = home.join("Library/Group Containers").join(&group);
    std::fs::create_dir_all(&dir)?;
    // Readable by this user only: the token is what keeps other local apps out of the bridge.
    let file = dir.join("bridge.json");
    let _ = std::fs::remove_file(&file);
    let mut out = std::os::unix::fs::OpenOptionsExt::mode(std::fs::OpenOptions::new().write(true).create_new(true), 0o600).open(&file)?;
    std::io::Write::write_all(&mut out, serde_json::json!({ "port": port, "token": token }).to_string().as_bytes())?;

    unsafe {
        let _: () = msg_send![class!(NSFileProviderManager), addDomain: &*domain(), completionHandler: &*log_error("add domain")];
    }
    // The other devices change their folders without telling us; let Finder re-check now and then.
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(30));
        refresh();
    });
    Ok(())
}

pub fn item_added(_node: &Node, _item: &lanshare_core::DriveItem) -> Result<()> {
    refresh();
    Ok(())
}

pub fn item_removed(_node: &Node, _item: &lanshare_core::DriveItem) -> Result<()> {
    refresh();
    Ok(())
}

/// Ask the extension to re-list the folders Finder has open (it reports changes and deletions).
fn refresh() {
    unsafe {
        let manager: Option<Retained<AnyObject>> = msg_send![class!(NSFileProviderManager), managerForDomain: &*domain()];
        let Some(manager) = manager else { return };
        let working_set = NSString::from_str("NSFileProviderWorkingSetContainerItemIdentifier");
        let _: () = msg_send![&*manager, signalEnumeratorForContainerItemIdentifier: &*working_set, completionHandler: &*log_error("refresh")];
    }
}

fn domain() -> Retained<AnyObject> {
    unsafe {
        let alloc: Allocated<AnyObject> = msg_send![class!(NSFileProviderDomain), alloc];
        msg_send![alloc, initWithIdentifier: &*NSString::from_str(DOMAIN_ID), displayName: &*NSString::from_str(DOMAIN_NAME)]
    }
}

fn log_error(what: &'static str) -> RcBlock<dyn Fn(*mut AnyObject)> {
    RcBlock::new(move |err: *mut AnyObject| {
        if !err.is_null() {
            let desc: Retained<NSString> = unsafe { msg_send![err, localizedDescription] };
            eprintln!("LAN Drive: {what}: {desc}");
        }
    })
}
