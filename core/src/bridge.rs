//! Localhost bridge for the macOS File Provider extension. The extension runs sandboxed in its
//! own process, so it asks the app over HTTP on 127.0.0.1 (with a bearer token) for listings and
//! file bytes. Item ids:
//!   `root` · `d/<peer>` (device folder) · `s/<peer>/<share>` (share folder) · `f/<peer>/<share>/<path>`
//!
//! GET /items?id=<folder id> -> JSON [Item]      GET /item?id=<id> -> JSON Item
//! GET /read?id=<file id>    -> raw bytes with Content-Length

use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

use crate::{Entry, Node};

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub parent: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
}

/// Bind on 127.0.0.1 (random port) and serve until the app exits. Returns the port.
pub async fn start(node: Arc<Node>, token: String) -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (node, token) = (node.clone(), token.clone());
            tokio::spawn(async move {
                if let Err(e) = handle(&node, &token, stream).await {
                    eprintln!("bridge: {e:#}");
                }
            });
        }
    });
    Ok(port)
}

async fn handle(node: &Arc<Node>, token: &str, stream: TcpStream) -> Result<()> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    stream.read_line(&mut line).await?;
    let target = line.split_whitespace().nth(1).context("bad request")?.to_string();
    let mut authorized = false;
    loop {
        let mut h = String::new();
        if stream.read_line(&mut h).await? == 0 || h.trim().is_empty() {
            break;
        }
        // Only this app and its extension know the token; anything else on localhost is refused.
        if h.trim().eq_ignore_ascii_case(&format!("authorization: bearer {token}")) {
            authorized = true;
        }
    }
    let mut out = stream.into_inner();
    if !authorized {
        return respond(&mut out, "401 Unauthorized", "text/plain", b"unauthorized").await;
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let id = query.split('&').find_map(|kv| kv.strip_prefix("id=")).map(percent_decode).transpose()?.unwrap_or_default();
    let res: Result<()> = async {
        match path {
            "/items" => {
                let items = children(node, &id).await?;
                respond(&mut out, "200 OK", "application/json", &serde_json::to_vec(&items)?).await
            }
            "/item" => {
                let item = item(node, &id).await?;
                respond(&mut out, "200 OK", "application/json", &serde_json::to_vec(&item)?).await
            }
            "/read" => read(node, &id, &mut out).await,
            _ => respond(&mut out, "404 Not Found", "text/plain", b"not found").await,
        }
    }
    .await;
    if let Err(e) = res {
        // Headers may already be out for /read; this only helps the other routes.
        let _ = respond(&mut out, "502 Bad Gateway", "text/plain", format!("{e:#}").as_bytes()).await;
    }
    Ok(())
}

async fn respond(out: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> Result<()> {
    out.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
    out.write_all(body).await?;
    Ok(out.shutdown().await?)
}

enum Id<'a> {
    Root,
    Device(&'a str),
    Share(&'a str, &'a str),
    File(&'a str, &'a str, &'a str),
}

fn parse(id: &str) -> Result<Id<'_>> {
    Ok(match id.split_once('/') {
        None if id == "root" => Id::Root,
        Some(("d", peer)) => Id::Device(peer),
        Some(("s", rest)) => {
            let (peer, share) = rest.split_once('/').context("bad id")?;
            Id::Share(peer, share)
        }
        Some(("f", rest)) => {
            let mut it = rest.splitn(3, '/');
            match (it.next(), it.next(), it.next()) {
                (Some(p), Some(s), Some(rel)) if !rel.is_empty() => Id::File(p, s, rel),
                _ => bail!("bad id"),
            }
        }
        _ => bail!("bad id: {id}"),
    })
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn from_entry(peer: &str, share: &str, parent_rel: &str, parent: &str, e: Entry) -> Item {
    let rel = if parent_rel.is_empty() { e.name.clone() } else { format!("{parent_rel}/{}", e.name) };
    Item { id: format!("f/{peer}/{share}/{rel}"), parent: parent.into(), name: e.name, is_dir: e.is_dir, size: e.size, modified: e.modified }
}

async fn children(node: &Node, id: &str) -> Result<Vec<Item>> {
    let items = node.drive_items();
    Ok(match parse(id)? {
        Id::Root => {
            // One folder per device that has at least one share in LAN Drive.
            let devices: BTreeMap<&str, String> = items.iter().map(|i| (i.peer.as_str(), file_name(i.path.parent().unwrap_or(&i.path)))).collect();
            devices.into_iter().map(|(peer, name)| Item { id: format!("d/{peer}"), parent: "root".into(), name, is_dir: true, size: 0, modified: 0 }).collect()
        }
        Id::Device(peer) => items
            .iter()
            .filter(|i| i.peer == peer)
            .map(|i| Item { id: format!("s/{peer}/{}", i.share), parent: id.into(), name: file_name(&i.path), is_dir: true, size: 0, modified: 0 })
            .collect(),
        Id::Share(peer, share) => node.remote_list(peer, share, "").await?.into_iter().map(|e| from_entry(peer, share, "", id, e)).collect(),
        Id::File(peer, share, rel) => node.remote_list(peer, share, rel).await?.into_iter().map(|e| from_entry(peer, share, rel, id, e)).collect(),
    })
}

async fn item(node: &Node, id: &str) -> Result<Item> {
    if let Id::File(peer, share, rel) = parse(id)? {
        // Find the entry in its parent's listing.
        let (parent_rel, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let parent = if parent_rel.is_empty() { format!("s/{peer}/{share}") } else { format!("f/{peer}/{share}/{parent_rel}") };
        let e = node.remote_list(peer, share, parent_rel).await?.into_iter().find(|e| e.name == name).context("not found")?;
        return Ok(from_entry(peer, share, parent_rel, &parent, e));
    }
    if id == "root" {
        return Ok(Item { id: "root".into(), parent: "root".into(), name: "LAN Drive".into(), is_dir: true, size: 0, modified: 0 });
    }
    let parent = match parse(id)? {
        Id::Share(peer, _) => format!("d/{peer}"),
        _ => "root".into(),
    };
    for candidate in [children(node, "root").await?, children(node, &parent).await.unwrap_or_default()] {
        if let Some(i) = candidate.into_iter().find(|i| i.id == id) {
            return Ok(i);
        }
    }
    bail!("not found")
}

async fn read(node: &Arc<Node>, id: &str, out: &mut TcpStream) -> Result<()> {
    let Id::File(peer, share, rel) = parse(id)? else { bail!("not a file") };
    let size = item(node, id).await?.size;
    out.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    // read_range takes a sync callback; hand chunks to this task through a channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (node, peer, share, rel) = (node.clone(), peer.to_string(), share.to_string(), rel.to_string());
    let fetch = tokio::spawn(async move { node.read_range(&peer, &share, &rel, 0, size, |c| Ok(tx.send(c.to_vec())?)).await });
    while let Some(chunk) = rx.recv().await {
        out.write_all(&chunk).await?;
    }
    fetch.await??;
    Ok(out.shutdown().await?)
}

fn percent_decode(s: &str) -> Result<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).context("bad escape")?;
            out.push(u8::from_str_radix(hex, 16)?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Ok(String::from_utf8(out)?)
}
