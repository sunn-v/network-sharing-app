//! Phase-0 spike: find peers on the LAN via mDNS and send one file over iroh (QUIC), LAN-only.
//!
//!   lanshare-spike serve [out_dir]      receive files into out_dir (default: ./received)
//!   lanshare-spike list                 show peers found on the LAN
//!   lanshare-spike send <file> [name]   send to the peer with that name (default: first found)
//!
//! Device name comes from LANSHARE_NAME (default: hostname).

use std::{path::Path, time::Instant};

use anyhow::{Context, Result, bail};
use iroh::{Endpoint, EndpointId, endpoint::Connection, endpoint::presets, endpoint_info::UserData};
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use n0_future::StreamExt;
use tokio::{fs, io::AsyncWriteExt, time::Duration};

const ALPN: &[u8] = b"lanshare/spike/0";
const SERVICE: &str = "lanshare";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["serve", rest @ ..] => serve(rest.first().copied().unwrap_or("received")).await,
        ["list"] => {
            let (ep, mdns) = bind().await?;
            for (id, name) in discover(&mdns, None, Duration::from_secs(5)).await {
                println!("{name}\t{id}");
            }
            ep.close().await;
            Ok(())
        }
        ["send", file, rest @ ..] => {
            let (ep, mdns) = bind().await?;
            let found = discover(&mdns, rest.first().copied(), Duration::from_secs(10)).await;
            let Some((id, name)) = found.into_iter().next() else { bail!("no peer found on the LAN") };
            println!("sending to {name} ({})", id.fmt_short());
            let conn = ep.connect(id, ALPN).await?;
            send_file(&conn, Path::new(file)).await?;
            ep.close().await;
            Ok(())
        }
        _ => bail!("usage: lanshare-spike serve [out_dir] | list | send <file> [name]"),
    }
}

fn device_name() -> String {
    std::env::var("LANSHARE_NAME")
        .ok()
        .or_else(|| hostname::get().ok().and_then(|h| h.into_string().ok()))
        .unwrap_or_else(|| "unknown".into())
}

/// LAN-only endpoint: the Minimal preset has no relay and no DNS lookup, so nothing leaves the network.
async fn bind() -> Result<(Endpoint, MdnsAddressLookup)> {
    let ep = Endpoint::builder(presets::Minimal).alpns(vec![ALPN.to_vec()]).bind().await?;
    let mdns = MdnsAddressLookup::builder().service_name(SERVICE).build(ep.id())?;
    ep.address_lookup()?.add(mdns.clone());
    ep.set_user_data_for_address_lookup(Some(UserData::try_from(device_name())?));
    Ok((ep, mdns))
}

/// Collect peers for `wait`, or return as soon as the one called `want` shows up.
async fn discover(mdns: &MdnsAddressLookup, want: Option<&str>, wait: Duration) -> Vec<(EndpointId, String)> {
    let mut events = mdns.subscribe().await;
    let mut found: Vec<(EndpointId, String)> = vec![];
    let deadline = tokio::time::sleep(wait);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            ev = events.next() => {
                let Some(DiscoveryEvent::Discovered { endpoint_info, .. }) = ev else { continue };
                let id = endpoint_info.endpoint_id;
                let name = endpoint_info.data.user_data().map(|u| u.to_string()).unwrap_or_default();
                if found.iter().any(|(f, _)| *f == id) { continue }
                if want.is_none_or(|w| w == name) {
                    found.push((id, name));
                    if want.is_some() { break }
                }
            }
        }
    }
    found
}

async fn serve(out_dir: &str) -> Result<()> {
    let (ep, _mdns) = bind().await?;
    fs::create_dir_all(out_dir).await?;
    println!("{} ready ({}), saving to {out_dir}", device_name(), ep.id().fmt_short());
    while let Some(incoming) = ep.accept().await {
        let out_dir = out_dir.to_string();
        tokio::spawn(async move {
            let res: Result<()> = async {
                let conn = incoming.await?;
                receive_file(&conn, Path::new(&out_dir)).await
            }
            .await;
            if let Err(e) = res {
                eprintln!("receive failed: {e:#}");
            }
        });
    }
    Ok(())
}

/// Wire format, one bi stream per file: u16 name_len | name | u64 size | bytes. Reply: b"ok".
async fn send_file(conn: &Connection, path: &Path) -> Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).context("bad file name")?;
    let mut file = fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(&(name.len() as u16).to_be_bytes()).await?;
    send.write_all(name.as_bytes()).await?;
    send.write_all(&size.to_be_bytes()).await?;
    let start = Instant::now();
    tokio::io::copy(&mut file, &mut send).await?;
    send.finish()?;
    let reply = recv.read_to_end(16).await?;
    if reply != b"ok" {
        bail!("peer rejected the file: {}", String::from_utf8_lossy(&reply));
    }
    report("sent", size, start);
    conn.close(0u32.into(), b"done");
    Ok(())
}

async fn receive_file(conn: &Connection, out_dir: &Path) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    let mut len = [0u8; 2];
    recv.read_exact(&mut len).await?;
    let mut name = vec![0u8; u16::from_be_bytes(len) as usize];
    recv.read_exact(&mut name).await?;
    let mut size = [0u8; 8];
    recv.read_exact(&mut size).await?;
    let size = u64::from_be_bytes(size);

    // Trust boundary: the name comes from another machine; keep only a plain file name.
    let target = match safe_name(&String::from_utf8_lossy(&name)) {
        Some(n) => out_dir.join(n),
        None => {
            send.write_all(b"bad name").await?;
            send.finish()?;
            bail!("rejected unsafe file name");
        }
    };
    // create_new: never overwrite a file that is already there.
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&target).await
        .with_context(|| format!("cannot create {}", target.display()))?;
    let start = Instant::now();
    let got = tokio::io::copy(&mut recv, &mut file).await?;
    file.flush().await?;
    if got != size {
        drop(file);
        fs::remove_file(&target).await.ok();
        bail!("short transfer: {got} of {size} bytes");
    }
    send.write_all(b"ok").await?;
    send.finish()?;
    report("received", size, start);
    println!("  -> {}", target.display());
    conn.closed().await;
    Ok(())
}

fn safe_name(name: &str) -> Option<&str> {
    let n = Path::new(name).file_name()?.to_str()?;
    (n == name && !n.is_empty()).then_some(n)
}

fn report(what: &str, bytes: u64, start: Instant) {
    let secs = start.elapsed().as_secs_f64().max(1e-6);
    println!("{what} {:.1} MB in {secs:.2}s = {:.1} MB/s", bytes as f64 / 1e6, bytes as f64 / 1e6 / secs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_tricks() {
        assert_eq!(safe_name("photo.jpg"), Some("photo.jpg"));
        for bad in ["../x", "a/b", "/etc/passwd", "..", ""] {
            assert_eq!(safe_name(bad), None, "{bad}");
        }
    }

    /// Two endpoints in one process, connected by direct address (mDNS is tested on real machines).
    #[tokio::test]
    async fn sends_one_file() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("lanshare-test-{}", std::process::id()));
        let out = dir.join("out");
        fs::create_dir_all(&out).await?;
        let src = dir.join("hello.bin");
        fs::write(&src, vec![7u8; 3_000_000]).await?;

        let (rx, _m1) = bind().await?;
        let (tx, _m2) = bind().await?;
        let rx_addr = rx.addr();
        let out2 = out.clone();
        let server = tokio::spawn(async move {
            let conn = rx.accept().await.unwrap().await?;
            receive_file(&conn, &out2).await
        });
        let conn = tx.connect(rx_addr, ALPN).await?;
        send_file(&conn, &src).await?;
        server.await??;
        assert_eq!(fs::read(out.join("hello.bin")).await?, fs::read(&src).await?);
        fs::remove_dir_all(&dir).await.ok();
        Ok(())
    }
}
