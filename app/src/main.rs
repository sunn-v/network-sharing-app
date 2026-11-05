// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Arc};

use lanshare_core::{Device, Entry, Node, RemoteShare};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

type N<'a> = State<'a, Arc<Node>>;
type R<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
struct Snapshot {
    id: String,
    name: String,
    devices: Vec<Device>,
}

#[derive(Serialize)]
struct LocalShare {
    id: String,
    name: String,
    path: String,
    enabled: bool,
}

#[tauri::command]
fn snapshot(node: N) -> Snapshot {
    Snapshot { id: node.id(), name: node.name(), devices: node.devices() }
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
    let base = app.path().download_dir().or_else(|_| app.path().home_dir().map(|h| h.join("Downloads")));
    let dest = base.map_err(|e| e.to_string())?.join("Network Sharing");
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    Ok((node.download(peer, share, paths, dest.clone()), dest.display().to_string()))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let node = tauri::async_runtime::block_on(Node::start(dir))?;
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
