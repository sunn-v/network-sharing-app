//! Core of the LAN folder-sharing app: device discovery (mDNS), pairing, shared folders,
//! browsing and downloading over iroh (QUIC). LAN-only: the Minimal preset has no relay and no DNS.
//!
//! Wire protocol: one request per bi stream, each message = u32 BE length + JSON.
//! `Read` replies with a JSON header, then the raw file bytes from the requested offset.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{
    Endpoint, EndpointId, SecretKey,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use iroh::{EndpointAddr, TransportAddr, address_lookup::MemoryLookup};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{broadcast, oneshot},
};

const ALPN: &[u8] = b"network-sharing/1";
const SERVICE: &str = "_netshare._udp.local.";
const MAX_MSG: u32 = 4 * 1024 * 1024;

// ---------- persisted state ----------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Share {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// Endpoint ids of paired devices that may see this share.
    pub peers: BTreeSet<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    name: String,
    secret: [u8; 32],
    /// Paired devices: endpoint id -> name.
    paired: BTreeMap<String, String>,
    shares: Vec<Share>,
}

// ---------- views sent to the UI ----------

#[derive(Serialize, Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub paired: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RemoteShare {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    Devices,
    PairRequest { id: String, name: String, code: String },
    Progress { job: u64, done: u64, total: u64, file: String },
    JobDone { job: u64, error: Option<String> },
}

// ---------- wire messages ----------

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum Req {
    Pair { name: String },
    Shares,
    List { share: String, path: String },
    Read { share: String, path: String, offset: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum Resp {
    Paired { accepted: bool },
    Shares { shares: Vec<RemoteShare> },
    List { entries: Vec<Entry> },
    Read { size: u64 },
    Error { msg: String },
}

// ---------- node ----------

pub struct Node {
    ep: Endpoint,
    /// Kept alive: it advertises us and browses for others on every interface.
    _mdns: ServiceDaemon,
    lookup: MemoryLookup,
    data_dir: PathBuf,
    cfg: Mutex<Config>,
    /// Devices seen on the LAN: id -> (name, online).
    seen: Mutex<BTreeMap<String, (String, bool)>>,
    pending_pairs: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    events: broadcast::Sender<Event>,
    next_job: Mutex<u64>,
    /// Runtime to spawn jobs on, so `download` works from non-async callers too.
    rt: tokio::runtime::Handle,
}

impl Node {
    /// Load (or create) the config in `data_dir`, bind the endpoint and start discovery and serving.
    pub async fn start(data_dir: PathBuf) -> Result<Arc<Node>> {
        fs::create_dir_all(&data_dir).await?;
        let cfg = match fs::read(data_dir.join("config.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes).context("config.json is corrupt")?,
            Err(_) => Config {
                name: hostname::get().ok().and_then(|h| h.into_string().ok()).unwrap_or_else(|| "Máy chưa đặt tên".into()),
                secret: SecretKey::generate().to_bytes(),
                ..Default::default()
            },
        };
        let ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&cfg.secret))
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;
        // Discovery: mdns-sd sends on every interface (the default-interface-only approach
        // misses peers when a VPN or a Hyper-V/WSL adapter owns the default route).
        let lookup = MemoryLookup::new();
        ep.address_lookup()?.add(lookup.clone());
        let mdns = ServiceDaemon::new()?;
        let id = ep.id().to_string();
        let port = ep.bound_sockets().iter().find(|a| a.is_ipv4()).map(|a| a.port()).context("no IPv4 socket")?;
        let instance = &id[..32]; // DNS labels max 63 bytes; the full id goes in TXT
        let props = [("id", id.as_str()), ("name", cfg.name.as_str())];
        mdns.register(ServiceInfo::new(SERVICE, instance, &format!("{instance}.local."), "", port, &props[..])?.enable_addr_auto())?;
        let events = mdns.browse(SERVICE)?;

        let node = Arc::new(Node {
            ep,
            _mdns: mdns,
            lookup,
            data_dir,
            cfg: Mutex::new(cfg),
            seen: Mutex::default(),
            pending_pairs: Mutex::default(),
            events: broadcast::channel(256).0,
            next_job: Mutex::new(1),
            rt: tokio::runtime::Handle::current(),
        });
        node.save().await?;
        tokio::spawn(node.clone().discover(events));
        tokio::spawn(node.clone().serve());
        Ok(node)
    }

    pub fn id(&self) -> String {
        self.ep.id().to_string()
    }

    pub fn name(&self) -> String {
        self.cfg.lock().unwrap().name.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev); // no subscriber is fine
    }

    async fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&*self.cfg.lock().unwrap())?;
        let tmp = self.data_dir.join("config.json.tmp");
        fs::write(&tmp, bytes).await?;
        fs::rename(&tmp, self.data_dir.join("config.json")).await?;
        Ok(())
    }

    pub fn devices(&self) -> Vec<Device> {
        let cfg = self.cfg.lock().unwrap();
        let seen = self.seen.lock().unwrap();
        let mut out: BTreeMap<String, Device> = cfg
            .paired
            .iter()
            .map(|(id, name)| (id.clone(), Device { id: id.clone(), name: name.clone(), online: false, paired: true }))
            .collect();
        for (id, (name, online)) in seen.iter() {
            let d = out.entry(id.clone()).or_insert(Device { id: id.clone(), name: name.clone(), online: false, paired: false });
            d.online = *online;
            if !name.is_empty() {
                d.name = name.clone();
            }
        }
        out.into_values().collect()
    }

    /// Six digits both screens show; equal codes mean both sides talk to the right key.
    pub fn pair_code(&self, peer: &str) -> Result<String> {
        let other: EndpointId = peer.parse()?;
        let mut keys = [*self.ep.id().as_bytes(), *other.as_bytes()];
        keys.sort();
        let h = Sha256::digest(keys.concat());
        let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
        Ok(format!("{:03} {:03}", n / 1000, n % 1000))
    }

    /// Ask `peer` to pair; resolves once the other side confirms or declines.
    pub async fn request_pair(&self, peer: &str) -> Result<bool> {
        let name = self.name();
        let Resp::Paired { accepted } = self.call(peer, &Req::Pair { name }).await? else { bail!("unexpected reply") };
        if accepted {
            let peer_name = self.seen.lock().unwrap().get(peer).map(|s| s.0.clone()).unwrap_or_default();
            self.cfg.lock().unwrap().paired.insert(peer.to_string(), peer_name);
            self.save().await?;
            self.emit(Event::Devices);
        }
        Ok(accepted)
    }

    /// Answer an incoming `Event::PairRequest`.
    pub fn answer_pair(&self, peer: &str, accept: bool) {
        if let Some(tx) = self.pending_pairs.lock().unwrap().remove(peer) {
            let _ = tx.send(accept);
        }
    }

    pub async fn unpair(&self, peer: &str) -> Result<()> {
        {
            let mut cfg = self.cfg.lock().unwrap();
            cfg.paired.remove(peer);
            for s in cfg.shares.iter_mut() {
                s.peers.remove(peer);
            }
        }
        self.save().await?;
        self.emit(Event::Devices);
        Ok(())
    }

    // ----- local shares -----

    pub fn shares(&self) -> Vec<Share> {
        self.cfg.lock().unwrap().shares.clone()
    }

    pub async fn add_share(&self, path: PathBuf, peer: Option<String>) -> Result<Share> {
        let path = fs::canonicalize(&path).await?;
        ensure!(fs::metadata(&path).await?.is_dir(), "not a folder");
        let share = {
            let mut cfg = self.cfg.lock().unwrap();
            if let Some(s) = cfg.shares.iter_mut().find(|s| s.path == path) {
                s.peers.extend(peer);
                s.clone()
            } else {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
                let id = Sha256::digest(path.to_string_lossy().as_bytes())[..8].iter().map(|b| format!("{b:02x}")).collect();
                let s = Share { id, name, path, peers: peer.into_iter().collect() };
                cfg.shares.push(s.clone());
                s
            }
        };
        self.save().await?;
        Ok(share)
    }

    pub async fn set_share_peer(&self, share: &str, peer: &str, enabled: bool) -> Result<()> {
        {
            let mut cfg = self.cfg.lock().unwrap();
            let s = cfg.shares.iter_mut().find(|s| s.id == share).context("no such share")?;
            if enabled {
                s.peers.insert(peer.to_string());
            } else {
                s.peers.remove(peer);
            }
        }
        self.save().await
    }

    pub async fn remove_share(&self, share: &str) -> Result<()> {
        self.cfg.lock().unwrap().shares.retain(|s| s.id != share);
        self.save().await
    }

    // ----- remote calls -----

    pub async fn remote_shares(&self, peer: &str) -> Result<Vec<RemoteShare>> {
        match self.call(peer, &Req::Shares).await? {
            Resp::Shares { shares } => Ok(shares),
            _ => bail!("unexpected reply"),
        }
    }

    pub async fn remote_list(&self, peer: &str, share: &str, path: &str) -> Result<Vec<Entry>> {
        match self.call(peer, &Req::List { share: share.into(), path: path.into() }).await? {
            Resp::List { entries } => Ok(entries),
            _ => bail!("unexpected reply"),
        }
    }

    /// Download `paths` (files or folders, relative to the share) into `dest`, keeping each
    /// selected item's name. Runs in the background; progress arrives as events. Returns the job id.
    pub fn download(self: &Arc<Self>, peer: String, share: String, paths: Vec<String>, dest: PathBuf) -> u64 {
        let job = {
            let mut n = self.next_job.lock().unwrap();
            *n += 1;
            *n
        };
        let node = self.clone();
        self.rt.spawn(async move {
            let res = node.run_download(job, &peer, &share, &paths, &dest).await;
            node.emit(Event::JobDone { job, error: res.err().map(|e| format!("{e:#}")) });
        });
        job
    }

    async fn run_download(&self, job: u64, peer: &str, share: &str, paths: &[String], dest: &Path) -> Result<()> {
        // 1. Expand folders into a flat file list: (remote path, local path, size).
        let mut files = vec![];
        for p in paths {
            let rel = safe_rel(p)?;
            let name = rel.file_name().context("empty path")?;
            let parent = parent_of(p);
            let entry = self
                .remote_list(peer, share, parent)
                .await?
                .into_iter()
                .find(|e| Path::new(&e.name) == Path::new(name))
                .with_context(|| format!("{p} not found"))?;
            self.collect(peer, share, p, &entry, dest.join(name), &mut files).await?;
        }
        let total: u64 = files.iter().map(|f| f.2).sum();
        let mut done = 0;
        // 2. Fetch each file, resuming a partial download if one exists.
        for (remote, local, size) in files {
            if let Some(dir) = local.parent() {
                fs::create_dir_all(dir).await?;
            }
            let part = local.with_file_name(format!("{}.nspart", local.file_name().unwrap().to_string_lossy()));
            let offset = fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0).min(size);
            let mut out = fs::OpenOptions::new().create(true).append(true).open(&part).await?;
            let (mut send, mut recv) = self.open(peer).await?;
            write_msg(&mut send, &Req::Read { share: share.into(), path: remote.clone(), offset }).await?;
            match read_msg::<Resp>(&mut recv).await? {
                Resp::Read { size: s } => ensure!(s == size, "{remote} changed on the other device"),
                Resp::Error { msg } => bail!(msg),
                _ => bail!("unexpected reply"),
            }
            done += offset;
            let mut buf = vec![0u8; 256 * 1024];
            let mut last = std::time::Instant::now();
            loop {
                let n = recv.read(&mut buf).await?.unwrap_or(0);
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).await?;
                done += n as u64;
                if last.elapsed() > Duration::from_millis(150) {
                    last = std::time::Instant::now();
                    self.emit(Event::Progress { job, done, total, file: remote.clone() });
                }
            }
            out.flush().await?;
            drop(out);
            let got = fs::metadata(&part).await?.len();
            ensure!(got == size, "{remote}: got {got} of {size} bytes, will resume next time");
            fs::rename(&part, unique_path(&local).await).await?;
        }
        self.emit(Event::Progress { job, done: total, total, file: String::new() });
        Ok(())
    }

    fn collect<'a>(
        &'a self,
        peer: &'a str,
        share: &'a str,
        remote: &'a str,
        entry: &'a Entry,
        local: PathBuf,
        files: &'a mut Vec<(String, PathBuf, u64)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if !entry.is_dir {
                files.push((remote.to_string(), local, entry.size));
                return Ok(());
            }
            for child in self.remote_list(peer, share, remote).await? {
                safe_rel(&child.name)?; // names come from another machine
                let child_remote = format!("{remote}/{}", child.name);
                let child_local = local.join(&child.name);
                self.collect(peer, share, &child_remote, &child, child_local, files).await?;
            }
            Ok(())
        })
    }

    async fn open(&self, peer: &str) -> Result<(SendStream, RecvStream)> {
        let id: EndpointId = peer.parse()?;
        let conn = self.ep.connect(id, ALPN).await.context("cannot reach device")?;
        Ok(conn.open_bi().await?)
    }

    async fn call(&self, peer: &str, req: &Req) -> Result<Resp> {
        let (mut send, mut recv) = self.open(peer).await?;
        write_msg(&mut send, req).await?;
        send.finish()?;
        match read_msg(&mut recv).await? {
            Resp::Error { msg } => bail!(msg),
            r => Ok(r),
        }
    }

    // ----- background tasks -----

    async fn discover(self: Arc<Self>, events: mdns_sd::Receiver<ServiceEvent>) {
        let me = self.id();
        while let Ok(ev) = events.recv_async().await {
            match ev {
                ServiceEvent::ServiceResolved(svc) => {
                    let Some(id) = svc.get_property_val_str("id").filter(|id| *id != me) else { continue };
                    let Ok(eid) = id.parse::<EndpointId>() else { continue };
                    let id = id.to_string();
                    let name = svc.get_property_val_str("name").unwrap_or_default().to_string();
                    let addrs = svc.get_addresses_v4().into_iter().map(|ip| TransportAddr::Ip((ip, svc.get_port()).into()));
                    self.lookup.add_endpoint_info(EndpointAddr::from_parts(eid, addrs));
                    let changed = self.seen.lock().unwrap().insert(id.clone(), (name.clone(), true)) != Some((name.clone(), true));
                    // Keep a paired device's stored name current.
                    if let Some(n) = self.cfg.lock().unwrap().paired.get_mut(&id) {
                        if !name.is_empty() {
                            *n = name;
                        }
                    }
                    if changed {
                        self.emit(Event::Devices);
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    let instance = fullname.split('.').next().unwrap_or_default().to_string();
                    for (id, d) in self.seen.lock().unwrap().iter_mut() {
                        if id.starts_with(&instance) {
                            d.1 = false;
                        }
                    }
                    self.emit(Event::Devices);
                }
                _ => {}
            }
        }
    }

    async fn serve(self: Arc<Self>) {
        while let Some(incoming) = self.ep.accept().await {
            let node = self.clone();
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                while let Ok((send, recv)) = conn.accept_bi().await {
                    let node = node.clone();
                    let conn = conn.clone();
                    tokio::spawn(async move {
                        if let Err(e) = node.handle(&conn, send, recv).await {
                            eprintln!("request failed: {e:#}");
                        }
                    });
                }
            });
        }
    }

    async fn handle(&self, conn: &Connection, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
        let peer = conn.remote_id().to_string();
        let req: Req = read_msg(&mut recv).await?;
        let resp = match self.answer(&peer, req, &mut send).await {
            Ok(None) => return Ok(()), // already streamed
            Ok(Some(r)) => r,
            Err(e) => Resp::Error { msg: format!("{e:#}") },
        };
        write_msg(&mut send, &resp).await?;
        send.finish()?;
        let _ = send.stopped().await;
        Ok(())
    }

    /// Every request except `Pair` needs a paired peer, and only sees shares enabled for it.
    async fn answer(&self, peer: &str, req: Req, send: &mut SendStream) -> Result<Option<Resp>> {
        if let Req::Pair { name } = req {
            let (tx, rx) = oneshot::channel();
            self.pending_pairs.lock().unwrap().insert(peer.to_string(), tx);
            self.emit(Event::PairRequest { id: peer.to_string(), name: name.clone(), code: self.pair_code(peer)? });
            let accepted = tokio::time::timeout(Duration::from_secs(120), rx).await.ok().and_then(|r| r.ok()).unwrap_or(false);
            self.pending_pairs.lock().unwrap().remove(peer);
            if accepted {
                self.cfg.lock().unwrap().paired.insert(peer.to_string(), name);
                self.save().await?;
                self.emit(Event::Devices);
            }
            return Ok(Some(Resp::Paired { accepted }));
        }
        ensure!(self.cfg.lock().unwrap().paired.contains_key(peer), "not paired");
        let visible: Vec<Share> = self.shares().into_iter().filter(|s| s.peers.contains(peer)).collect();
        let root_of = |id: &str| visible.iter().find(|s| s.id == id).map(|s| s.path.clone()).context("folder is not shared with you");
        Ok(Some(match req {
            Req::Pair { .. } => unreachable!(),
            Req::Shares => Resp::Shares { shares: visible.iter().map(|s| RemoteShare { id: s.id.clone(), name: s.name.clone() }).collect() },
            Req::List { share, path } => {
                let dir = resolve(&root_of(&share)?, &path).await?;
                let mut entries = vec![];
                let mut rd = fs::read_dir(&dir).await?;
                while let Some(e) = rd.next_entry().await? {
                    let Ok(m) = fs::metadata(e.path()).await else { continue }; // broken link
                    let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                    let name = e.file_name().to_string_lossy().into_owned();
                    entries.push(Entry { name, is_dir: m.is_dir(), size: if m.is_dir() { 0 } else { m.len() }, modified });
                }
                entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                Resp::List { entries }
            }
            Req::Read { share, path, offset } => {
                let file = resolve(&root_of(&share)?, &path).await?;
                let mut f = fs::File::open(&file).await?;
                let size = f.metadata().await?.len();
                write_msg(send, &Resp::Read { size }).await?;
                tokio::io::AsyncSeekExt::seek(&mut f, std::io::SeekFrom::Start(offset.min(size))).await?;
                tokio::io::copy(&mut f, send).await?;
                send.finish()?;
                let _ = send.stopped().await;
                return Ok(None);
            }
        }))
    }
}

// ---------- helpers ----------

async fn write_msg<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_msg<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len);
    ensure!(len <= MAX_MSG, "message too large");
    let mut buf = vec![0u8; len as usize];
    recv.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

/// A path from another machine: only plain names separated by `/`, nothing that climbs or is absolute.
fn safe_rel(p: &str) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in p.split('/').filter(|s| !s.is_empty()) {
        let mut comps = Path::new(part).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(c)), None) if c == part && !part.contains('\\') && !part.contains(':') => out.push(c),
            _ => bail!("unsafe path: {p}"),
        }
    }
    Ok(out)
}

/// Resolve `rel` inside `root`; refuses anything (including symlinks) that ends up outside it.
async fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let full = fs::canonicalize(root.join(safe_rel(rel)?)).await.context("not found")?;
    ensure!(full.starts_with(fs::canonicalize(root).await?), "outside the shared folder");
    Ok(full)
}

fn parent_of(p: &str) -> &str {
    p.trim_end_matches('/').rsplit_once('/').map_or("", |(a, _)| a)
}

/// `photo.jpg` -> `photo (2).jpg` if taken: never overwrite a file the user already has.
async fn unique_path(p: &Path) -> PathBuf {
    if fs::metadata(p).await.is_err() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 2.. {
        let c = p.with_file_name(format!("{stem} ({i}){ext}"));
        if fs::metadata(&c).await.is_err() {
            return c;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests;
