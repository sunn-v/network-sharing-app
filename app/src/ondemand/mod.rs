//! LAN Drive "on demand": the OS shows every shared file in Explorer/Finder, and its bytes are
//! fetched only when something opens it. Windows uses the Cloud Files API, macOS a File Provider
//! extension. Elsewhere (Linux, or a macOS build without the extension) LAN Drive falls back to
//! full copies made by the core.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
use windows as os;
#[cfg(target_os = "macos")]
use macos as os;

/// Without an OS layer (Linux), LAN Drive falls back to full copies made by the core.
#[cfg(not(any(windows, target_os = "macos")))]
mod os {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use anyhow::Result;
    use lanshare_core::{DriveItem, Node};

    pub fn available() -> bool {
        false
    }
    pub fn drive_root(home: &Path) -> PathBuf {
        home.join("LAN Drive")
    }
    pub fn start(_: Arc<Node>) -> Result<()> {
        Ok(())
    }
    pub fn item_added(_: &Node, _: &DriveItem) -> Result<()> {
        Ok(())
    }
    pub fn item_removed(_: &Node, _: &DriveItem) -> Result<()> {
        Ok(())
    }
}

pub use os::{available, drive_root, item_added, item_removed, start};
