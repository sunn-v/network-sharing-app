use super::*;

#[test]
fn safe_rel_rejects_escapes() {
    assert_eq!(safe_rel("a/b.txt").unwrap(), PathBuf::from("a").join("b.txt"));
    assert_eq!(safe_rel("").unwrap(), PathBuf::new());
    // A leading "/" is ignored, so "/etc/passwd" stays inside the share.
    assert_eq!(safe_rel("/etc/passwd").unwrap(), PathBuf::from("etc").join("passwd"));
    for bad in ["../x", "a/../../x", "a/..", "C:\\x", "a\\..\\b", "c:x"] {
        assert!(safe_rel(bad).is_err(), "{bad}");
    }
}

#[test]
fn parent_of_works() {
    assert_eq!(parent_of("a/b/c"), "a/b");
    assert_eq!(parent_of("c"), "");
}

async fn wait_until(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out");
}

/// Two nodes on this host: find each other over mDNS, pair, share, browse, download a folder.
#[tokio::test(flavor = "multi_thread")]
async fn pair_share_browse_download() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let a = Node::start(tmp.path().join("a"), tmp.path().join("drive-a")).await?;
    let b = Node::start(tmp.path().join("b"), tmp.path().join("drive-b")).await?;
    let (aid, bid) = (a.id(), b.id());

    wait_until(|| a.devices().iter().any(|d| d.id == bid && d.online)).await;
    assert_eq!(a.pair_code(&bid)?, b.pair_code(&aid)?);

    // Unpaired devices see nothing.
    assert!(a.remote_shares(&bid).await.is_err());

    // B accepts whatever pair request arrives.
    let mut rx = b.subscribe();
    let b2 = b.clone();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Event::PairRequest { id, .. } = ev {
                b2.answer_pair(&id, true);
            }
        }
    });
    assert!(a.request_pair(&bid).await?);
    wait_until(|| b.devices().iter().any(|d| d.id == aid && d.paired)).await;

    // B shares a folder with A, and a second folder with nobody.
    let src = tmp.path().join("Ảnh 2026");
    fs::create_dir_all(src.join("Đà Lạt")).await?;
    fs::write(src.join("Đà Lạt/1.jpg"), vec![1u8; 700_000]).await?;
    fs::write(src.join("note.txt"), b"hello").await?;
    fs::create_dir_all(tmp.path().join("secret")).await?;
    let share = b.add_share(src.clone(), Some(aid.clone())).await?;
    b.add_share(tmp.path().join("secret"), None).await?;

    let shares = a.remote_shares(&bid).await?;
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].name, "Ảnh 2026");

    let top = a.remote_list(&bid, &share.id, "").await?;
    assert_eq!(top.iter().map(|e| (e.name.as_str(), e.is_dir)).collect::<Vec<_>>(), [("Đà Lạt", true), ("note.txt", false)]);
    assert!(a.remote_list(&bid, &share.id, "../").await.is_err());
    assert!(a.remote_list(&bid, "nope", "").await.is_err());

    // Download the subfolder and a file; a pre-existing file must not be overwritten.
    let dest = tmp.path().join("dl");
    fs::create_dir_all(&dest).await?;
    fs::write(dest.join("note.txt"), b"mine").await?;
    let mut ev = a.subscribe();
    let job = a.download(bid.clone(), share.id.clone(), vec!["Đà Lạt".into(), "note.txt".into()], dest.clone());
    loop {
        if let Event::JobDone { job: j, error } = ev.recv().await? {
            if j == job {
                assert_eq!(error, None);
                break;
            }
        }
    }
    assert_eq!(fs::read(dest.join("Đà Lạt/1.jpg")).await?, vec![1u8; 700_000]);
    assert_eq!(fs::read(dest.join("note.txt")).await?, b"mine");
    assert_eq!(fs::read(dest.join("note (2).txt")).await?, b"hello");

    // Turning the share off for A hides it again.
    b.set_share_peer(&share.id, &aid, false).await?;
    assert!(a.remote_shares(&bid).await?.is_empty());
    Ok(())
}

/// A half-finished `.nspart` is continued, not restarted.
#[tokio::test(flavor = "multi_thread")]
async fn resumes_partial_download() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let a = Node::start(tmp.path().join("a"), tmp.path().join("drive-a")).await?;
    let b = Node::start(tmp.path().join("b"), tmp.path().join("drive-b")).await?;
    let (aid, bid) = (a.id(), b.id());
    wait_until(|| a.devices().iter().any(|d| d.id == bid && d.online)).await;
    // Pair directly in config, as if done earlier.
    a.cfg.lock().unwrap().paired.insert(bid.clone(), "b".into());
    b.cfg.lock().unwrap().paired.insert(aid.clone(), "a".into());

    let src = tmp.path().join("src");
    fs::create_dir_all(&src).await?;
    let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(src.join("big.bin"), &data).await?;
    let share = b.add_share(src.clone(), Some(aid)).await?;

    let dest = tmp.path().join("dl");
    fs::create_dir_all(&dest).await?;
    let mtime = fs::metadata(src.join("big.bin")).await?.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let part = dest.join(format!("big.bin.{mtime}.nspart"));
    fs::write(&part, &data[..400_000]).await?;
    let mut ev = a.subscribe();
    // Called from a plain thread, like a sync Tauri command (regression: used to panic).
    let a2 = a.clone();
    let d2 = dest.clone();
    let job = std::thread::spawn(move || a2.download(bid, share.id, vec!["big.bin".into()], d2)).join().unwrap();
    loop {
        if let Event::JobDone { job: j, error } = ev.recv().await? {
            if j == job {
                assert_eq!(error, None);
                break;
            }
        }
    }
    assert_eq!(fs::read(dest.join("big.bin")).await?, data);
    assert!(fs::metadata(&part).await.is_err());
    Ok(())
}

#[test]
fn folder_name_is_safe() {
    assert_eq!(folder_name("Mac mini: M4/a", "x"), "Mac mini_ M4_a");
    assert_eq!(folder_name("..", "fallback"), "fallback");
    assert_eq!(folder_name("Ảnh 2026", "x"), "Ảnh 2026");
}

/// Rename shows up on the other side; LAN Drive mirrors a share and picks up changes.
#[tokio::test(flavor = "multi_thread")]
async fn rename_and_lan_drive() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let a = Node::start(tmp.path().join("a"), tmp.path().join("drive-a")).await?;
    let b = Node::start(tmp.path().join("b"), tmp.path().join("drive-b")).await?;
    let (aid, bid) = (a.id(), b.id());
    wait_until(|| a.devices().iter().any(|d| d.id == bid && d.online)).await;

    b.set_name("Máy B").await?;
    wait_until(|| a.devices().iter().any(|d| d.id == bid && d.name == "Máy B")).await;
    assert!(b.set_name("   ").await.is_err());

    a.cfg.lock().unwrap().paired.insert(bid.clone(), "Máy B".into());
    b.cfg.lock().unwrap().paired.insert(aid.clone(), "a".into());
    let src = tmp.path().join("Tài liệu");
    fs::create_dir_all(src.join("sub")).await?;
    fs::write(src.join("sub/a.txt"), b"v1").await?;
    let share = b.add_share(src.clone(), Some(aid.clone())).await?;

    a.add_to_drive(&bid, &share.id, &share.name).await?;
    let mirrored = tmp.path().join("drive-a/Máy B/Tài liệu/sub/a.txt");
    let synced = |a: &Arc<Node>| a.drive().first().is_some_and(|d| d.status.state == "synced");
    wait_until(|| synced(&a)).await;
    assert_eq!(fs::read(&mirrored).await?, b"v1");

    // Change the source (new size and mtime), run another pass, expect the new content.
    fs::write(src.join("sub/a.txt"), b"version 2").await?;
    std::fs::File::options().write(true).open(src.join("sub/a.txt"))?.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))?;
    let item = a.cfg.lock().unwrap().drive[0].clone();
    a.sync_item(&item).await;
    assert_eq!(fs::read(&mirrored).await?, b"version 2");
    assert!(synced(&a));

    a.remove_from_drive(&bid, &share.id).await?;
    assert!(a.drive().is_empty());
    assert!(fs::metadata(&mirrored).await.is_ok(), "removing from LAN Drive keeps the files");
    Ok(())
}
