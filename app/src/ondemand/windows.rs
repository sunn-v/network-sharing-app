//! LAN Drive on Windows through the Cloud Files API (the one OneDrive uses): `~/LAN Drive` is a
//! sync root, every remote file is a placeholder, and its bytes are fetched when something opens it.
//!
//! Layout: `LAN Drive\<device>\<share>\...`. Device and share folders are created by us; folders
//! inside a share are filled by `fetch_placeholders` the first time Explorer opens them.
//! Each placeholder carries a blob "peer\nshare\nrelative/path" so callbacks know what to fetch.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use cloud_filter::{
    error::{CResult, CloudErrorKind},
    filter::{info, ticket, Request, SyncFilter},
    metadata::Metadata,
    placeholder_file::PlaceholderFile,
    root::{HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder, SyncRootInfo},
    utility::WriteAt,
};
use lanshare_core::{DriveItem, Node};
use nt_time::FileTime;

const PROVIDER: &str = "NetworkSharing";
/// Bytes handed to Windows per write; must be a multiple of 4 KiB (except at end of file).
const CHUNK: usize = 1024 * 1024;

pub fn available() -> bool {
    true
}

pub fn drive_root(home: &Path) -> PathBuf {
    home.join("LAN Drive")
}

pub fn start(node: Arc<Node>) -> Result<()> {
    let root = node.drive_root().to_path_buf();
    let id = SyncRootIdBuilder::new(PROVIDER).user_security_id(SecurityId::current_user()?).build();
    if !id.is_registered()? {
        // A folder left by the old full-copy LAN Drive: keep its files, out of the way.
        if root.exists() {
            let old = free_name(&root.with_file_name("LAN Drive (bản cũ)"));
            std::fs::rename(&root, &old).with_context(|| format!("cannot move old {}", root.display()))?;
        }
        std::fs::create_dir_all(&root)?;
        let icon = format!("{},0", std::env::current_exe()?.display());
        id.register(
            SyncRootInfo::default()
                .with_display_name("LAN Drive")
                .with_hydration_type(HydrationType::Progressive)
                .with_population_type(PopulationType::Full)
                .with_icon(icon)
                .with_version(env!("CARGO_PKG_VERSION"))
                .with_path(&root)?,
        )
        .context("cannot register LAN Drive with Windows")?;
    }
    let conn = Session::new().connect(&root, Filter { node: node.clone() }).context("cannot connect LAN Drive")?;
    // The connection must live as long as the app; Windows drops it when the process exits.
    std::mem::forget(conn);
    for item in node.drive_items() {
        if let Err(e) = item_added(&node, &item) {
            eprintln!("LAN Drive: {}: {e:#}", item.path.display());
        }
    }
    Ok(())
}

/// Create the device and share folders for a new LAN Drive item (no-op if they exist).
pub fn item_added(node: &Node, item: &DriveItem) -> Result<()> {
    let device_dir = item.path.parent().context("bad LAN Drive path")?;
    if !device_dir.exists() {
        // We add share folders ourselves, so Windows must never ask us to fill this one.
        PlaceholderFile::new(device_dir.file_name().context("bad device folder")?)
            .has_no_children()
            .mark_in_sync()
            .metadata(Metadata::directory())
            .blob(blob(&item.peer, "", ""))
            .create::<&Path>(node.drive_root())?;
    }
    if !item.path.exists() {
        PlaceholderFile::new(item.path.file_name().context("bad share folder")?)
            .mark_in_sync()
            .metadata(Metadata::directory())
            .blob(blob(&item.peer, &item.share, ""))
            .create::<&Path>(device_dir)?;
    }
    Ok(())
}

/// Take a removed item's folder out of LAN Drive. Its files are only a cache of the other
/// device's copies; anything the user saved inside that folder goes with it (the UI warns).
pub fn item_removed(_node: &Node, item: &DriveItem) -> Result<()> {
    if item.path.exists() {
        std::fs::remove_dir_all(&item.path).with_context(|| format!("cannot remove {}", item.path.display()))?;
    }
    if let Some(device_dir) = item.path.parent() {
        let _ = std::fs::remove_dir(device_dir); // only succeeds when empty
    }
    Ok(())
}

struct Filter {
    node: Arc<Node>,
}

impl SyncFilter for Filter {
    fn fetch_data(&self, request: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
        let (peer, share, rel) = parse(request.file_blob()).ok_or(CloudErrorKind::InvalidRequest)?;
        let need = info.required_file_range();
        let end = need.end.max(info.optional_file_range().end);
        let mut pos = need.start;
        let mut buf: Vec<u8> = Vec::with_capacity(CHUNK * 2);
        let res = self.node.runtime().block_on(self.node.read_range(&peer, &share, &rel, need.start, end - need.start, |chunk| {
            buf.extend_from_slice(chunk);
            if buf.len() >= CHUNK {
                let n = buf.len() / 4096 * 4096;
                ticket.write_at(&buf[..n], pos)?;
                pos += n as u64;
                buf.drain(..n);
            }
            Ok(())
        }));
        let res = res.and_then(|_| if buf.is_empty() { Ok(()) } else { Ok(ticket.write_at(&buf, pos)?) });
        res.map_err(|e| {
            eprintln!("LAN Drive: fetch {rel}: {e:#}");
            if self.node.devices().iter().any(|d| d.id == peer && d.online) { CloudErrorKind::Unsuccessful } else { CloudErrorKind::NetworkUnavailable }
        })
    }

    fn fetch_placeholders(&self, request: Request, ticket: ticket::FetchPlaceholders, _info: info::FetchPlaceholders) -> CResult<()> {
        // The root and device folders: their children are created by `item_added`.
        let Some((peer, share, rel)) = parse(request.file_blob()).filter(|(_, share, _)| !share.is_empty()) else {
            return ticket.pass_with_placeholder(&mut []).map_err(|_| CloudErrorKind::Unsuccessful);
        };
        let entries = self.node.runtime().block_on(self.node.remote_list(&peer, &share, &rel)).map_err(|e| {
            eprintln!("LAN Drive: list {rel}: {e:#}");
            CloudErrorKind::NetworkUnavailable
        })?;
        let mut placeholders: Vec<PlaceholderFile> = entries
            .into_iter()
            .filter(|e| windows_name_ok(&e.name) && !e.name.ends_with(".nspart"))
            .filter_map(|e| {
                let child = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
                let id = blob(&peer, &share, &child);
                if id.len() > 4000 {
                    return None; // path too long for a placeholder identity
                }
                let t = FileTime::from_unix_time(e.modified as i64).unwrap_or_else(|_| FileTime::now());
                let meta = if e.is_dir { Metadata::directory() } else { Metadata::file().size(e.size) };
                Some(PlaceholderFile::new(&e.name).mark_in_sync().metadata(meta.created(t).written(t).changed(t).accessed(t)).blob(id))
            })
            .collect();
        ticket.pass_with_placeholder(&mut placeholders).map_err(|e| {
            eprintln!("LAN Drive: list {rel}: {e}");
            CloudErrorKind::Unsuccessful
        })
    }

    // LAN Drive is a read-only window on the other device: local deletes, renames and
    // "Free up space" only touch this machine, so allow them.
    fn dehydrate(&self, _request: Request, ticket: ticket::Dehydrate, _info: info::Dehydrate) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn delete(&self, _request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn rename(&self, _request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }
}

fn blob(peer: &str, share: &str, rel: &str) -> Vec<u8> {
    format!("{peer}\n{share}\n{rel}").into_bytes()
}

fn parse(blob: &[u8]) -> Option<(String, String, String)> {
    let s = std::str::from_utf8(blob).ok()?;
    let mut it = s.splitn(3, '\n');
    Some((it.next()?.to_string(), it.next()?.to_string(), it.next()?.to_string()))
}

/// Names from macOS can hold characters Windows forbids; such entries are left out.
fn windows_name_ok(name: &str) -> bool {
    !name.is_empty() && !name.ends_with(['.', ' ']) && !name.chars().any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c))
}

fn free_name(p: &Path) -> PathBuf {
    (1..).map(|i| if i == 1 { p.to_path_buf() } else { PathBuf::from(format!("{} {i}", p.display())) }).find(|c| !c.exists()).unwrap()
}
