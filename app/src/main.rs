// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use lanshare_core::{Device, DriveView, Entry, Node, RemoteShare};
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

mod ondemand;

type N<'a> = State<'a, Arc<Node>>;
type R<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
struct Snapshot {
    id: String,
    name: String,
    drive_root: String,
    /// LAN Drive shows files on demand (Windows, macOS with the extension) instead of copying them.
    on_demand: bool,
    /// Why on-demand LAN Drive could not start, if it could not.
    drive_error: Option<String>,
    devices: Vec<Device>,
}

/// Set once at startup when the on-demand layer fails.
struct DriveError(Mutex<Option<String>>);

#[derive(Serialize)]
struct LocalShare {
    id: String,
    name: String,
    path: String,
    enabled: bool,
}

#[tauri::command]
fn snapshot(node: N, drive_error: State<DriveError>) -> Snapshot {
    Snapshot {
        id: node.id(),
        name: node.name(),
        drive_root: node.drive_root().display().to_string(),
        on_demand: ondemand::available(),
        drive_error: drive_error.0.lock().unwrap().clone(),
        devices: node.devices(),
    }
}

#[tauri::command]
async fn set_name(node: N<'_>, name: String) -> R<()> {
    node.set_name(&name).await.map_err(err)
}

#[tauri::command]
fn drive_list(node: N) -> Vec<DriveView> {
    node.drive()
}

#[tauri::command]
async fn add_to_drive(node: N<'_>, peer: String, share: String, name: String) -> R<()> {
    let item = node.add_to_drive(&peer, &share, &name).await.map_err(err)?;
    ondemand::item_added(&node, &item).map_err(err)
}

#[tauri::command]
async fn remove_from_drive(node: N<'_>, peer: String, share: String) -> R<()> {
    let item = node.drive_items().into_iter().find(|i| i.peer == peer && i.share == share);
    node.remove_from_drive(&peer, &share).await.map_err(err)?;
    match item {
        Some(item) if ondemand::available() => ondemand::item_removed(&node, &item).map_err(err),
        _ => Ok(()),
    }
}

/// Open a folder in Finder/Explorer. Only LAN Drive and the download folder, never arbitrary paths.
#[tauri::command]
fn open_path(app: tauri::AppHandle, node: N, path: String) -> R<()> {
    let path = PathBuf::from(path);
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("không mở được đường dẫn này".into());
    }
    let allowed = [node.drive_root().to_path_buf(), downloads(&app)?];
    if !allowed.iter().any(|root| path.starts_with(root)) {
        return Err("không mở được đường dẫn này".into());
    }
    if path.starts_with(downloads(&app)?) || !ondemand::available() {
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    } else if !path.exists() {
        // macOS names the LAN Drive folder itself; it may not have existed at startup.
        let root = ondemand::drive_root(&app.path().home_dir().map_err(|e| e.to_string())?);
        if !root.exists() {
            return Err("LAN Drive chưa sẵn sàng, thử lại sau vài giây".into());
        }
        return app.opener().open_path(root.display().to_string(), None::<&str>).map_err(|e| e.to_string());
    }
    app.opener().open_path(path.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

fn downloads(app: &tauri::AppHandle) -> R<PathBuf> {
    let base = app.path().download_dir().or_else(|_| app.path().home_dir().map(|h| h.join("Downloads")));
    Ok(base.map_err(|e| e.to_string())?.join("Network Sharing"))
}

#[tauri::command]
fn pair_code(node: N, peer: String) -> R<String> {
    node.pair_code(&peer).map_err(err)
}

#[tauri::command]
async fn request_pair(node: N<'_>, peer: String) -> R<bool> {
    node.request_pair(&peer).await.map_err(err)
}

#[tauri::command]
fn answer_pair(node: N, peer: String, accept: bool) {
    node.answer_pair(&peer, accept)
}

#[tauri::command]
async fn unpair(node: N<'_>, peer: String) -> R<()> {
    node.unpair(&peer).await.map_err(err)
}

#[tauri::command]
fn local_shares(node: N, peer: String) -> Vec<LocalShare> {
    node.shares()
        .into_iter()
        .map(|s| LocalShare { enabled: s.peers.contains(&peer), id: s.id, name: s.name, path: s.path.display().to_string() })
        .collect()
}

#[tauri::command]
async fn add_share(node: N<'_>, path: String, peer: String) -> R<()> {
    node.add_share(PathBuf::from(path), Some(peer)).await.map(|_| ()).map_err(err)
}

#[tauri::command]
async fn set_share_peer(node: N<'_>, share: String, peer: String, enabled: bool) -> R<()> {
    node.set_share_peer(&share, &peer, enabled).await.map_err(err)
}

#[tauri::command]
async fn remote_shares(node: N<'_>, peer: String) -> R<Vec<RemoteShare>> {
    node.remote_shares(&peer).await.map_err(err)
}

#[tauri::command]
async fn remote_list(node: N<'_>, peer: String, share: String, path: String) -> R<Vec<Entry>> {
    node.remote_list(&peer, &share, &path).await.map_err(err)
}

/// Downloads land in ~/Downloads/Network Sharing.
#[tauri::command]
fn download(app: tauri::AppHandle, node: N, peer: String, share: String, paths: Vec<String>) -> R<(u64, String)> {
    let dest = downloads(&app)?;
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    Ok((node.download(peer, share, paths, dest.clone()), dest.display().to_string()))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let drive = ondemand::drive_root(&app.path().home_dir()?);
            let on_demand = ondemand::available();
            let node = tauri::async_runtime::block_on(Node::start(dir, drive, !on_demand))?;
            let drive_error = on_demand.then(|| ondemand::start(node.clone()).err()).flatten().map(|e| format!("{e:#}"));
            if let Some(e) = &drive_error {
                eprintln!("LAN Drive: {e}");
            }
            app.manage(DriveError(Mutex::new(drive_error)));
            // Forward core events to the window.
            let mut rx = node.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = rx.recv().await {
                    let _ = handle.emit("core", ev);
                }
            });
            app.manage(node);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            set_name,
            drive_list,
            add_to_drive,
            remove_from_drive,
            open_path,
            pair_code,
            request_pair,
            answer_pair,
            unpair,
            local_shares,
            add_share,
            set_share_peer,
            remote_shares,
            remote_list,
            download
        ])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
