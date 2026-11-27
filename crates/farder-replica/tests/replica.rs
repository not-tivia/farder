//! A replica against the REAL farder-server binary: it joins with its own key, is refused until the
//! owner allows it, then pulls a verified copy that really contains the server's data.
use farder_crypto::event_log::{invite_code_hash, DeviceCert, Event, EventPayload};
use farder_crypto::identity::Keypair;
use farder_protocol::server::*;
use farder_replica::{load_or_create_key, pull_once, sync_files, verify_copy, FileSync, PullError, Session};
use std::net::SocketAddr;
use std::time::Duration;

struct Srv { child: std::process::Child, addr: SocketAddr, token: String, _dir: tempfile::TempDir }
impl Drop for Srv { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }

async fn real_server() -> Srv {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/farder-server");
    assert!(bin.exists(), "build the server first: cargo build -p farder-server");
    let port = { let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); s.local_addr().unwrap().port() };
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let out = std::fs::File::create(&log).unwrap();
    let child = std::process::Command::new(&bin)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--name", "Replica Test", "--db"]).arg(dir.path().join("s.db"))
        .arg("--storage-dir").arg(dir.path().join("files")).arg("--data-dir").arg(dir.path().join("sd"))
        .stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = std::fs::read_to_string(&log).unwrap_or_default();
        if t.contains("Server listening") {
            let clean: String = t.chars().filter(|c| !c.is_control() || *c == '\n').collect();
            let tok = clean.split("token instead: ").nth(1).map(|x| x.split_whitespace().next().unwrap().trim_matches(|c: char| !c.is_ascii_hexdigit()).to_string()).unwrap();
            return Srv { child, addr: format!("127.0.0.1:{port}").parse().unwrap(), token: tok, _dir: dir };
        }
    }
    panic!("server did not start");
}

struct Seeded { srv: Srv, owner: Keypair, o: Session, sid: String, code: String }

/// A real server with an owner, a normal invite (with its signed record), a message and a database-only channel.
async fn seeded() -> Seeded {
    let srv = real_server().await;
    // The owner claims the server, makes a normal invite (with its signed record), and posts a message.
    let owner = Keypair::generate();
    let dev = Keypair::generate();
    let mut o = Session::connect(srv.addr, None, &owner, None, Some(srv.token.clone())).await.unwrap();
    let ServerResponse::ServerInfo { server_id: Some(sid), channels, .. } = o.ask(ServerRequest::GetServerInfo).await.unwrap() else { panic!() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let da = Event::next(&dev, owner.public_key(), sid.clone(), None, 0, now, EventPayload::DeviceAuthorized { cert: DeviceCert::create(&owner, &dev.public_key(), now) });
    assert!(!matches!(o.ask(ServerRequest::SubmitEvent { event: da.clone() }).await.unwrap(), ServerResponse::Error { .. }));
    let ServerResponse::InviteCreated { code } = o.ask(ServerRequest::CreateInvite { max_uses: Some(1), expires_in_secs: None, target_channel: None }).await.unwrap() else { panic!() };
    let inv = Event::next(&dev, owner.public_key(), sid.clone(), Some(&da), da.core.lamport, now, EventPayload::InviteCreated { code_hash: invite_code_hash(&code), max_uses: 1, expires_at: now + 3600, requires_approval: false });
    assert!(!matches!(o.ask(ServerRequest::SubmitEvent { event: inv }).await.unwrap(), ServerResponse::Error { .. }));
    let general = channels.iter().find(|c| c.name == "general").unwrap().id;
    assert!(matches!(o.ask(ServerRequest::SendMessage { channel_id: general, content: "history that must survive".into(), reply_to: None, attachment_ids: vec![] }).await.unwrap(), ServerResponse::MessageSent { .. }));
    assert!(matches!(o.ask(ServerRequest::CreateChannel { name: "only-in-the-database".into(), channel_type: ChannelType::Text, category_id: None, position: None }).await.unwrap(), ServerResponse::Ok));

    Seeded { srv, owner, o, sid, code }
}

#[tokio::test]
async fn a_replica_is_refused_until_allowed_then_pulls_a_verified_copy_of_the_real_server() {
    let Seeded { srv, owner: _owner, mut o, sid, code } = seeded().await;
    // The replica: its own key, joins with the invite.
    let work = tempfile::tempdir().unwrap();
    let (rkey, fresh) = load_or_create_key(&work.path().join("replica.key")).unwrap();
    assert!(fresh);
    let mut r = Session::connect(srv.addr, None, &rkey, Some(code.clone()), None).await.unwrap();
    r.join_history(&rkey, &code).await.unwrap();
    // It names itself, so members see "Backup copy" in the member list rather than "Anonymous".
    r.set_name(&rkey, "Backup copy").await.unwrap();
    let ServerResponse::Members { members } = o.ask(ServerRequest::GetMembers).await.unwrap() else { panic!("members") };
    assert!(members.iter().any(|m| m.public_key == rkey.public_key() && m.display_name == "Backup copy"), "{members:?}");
    let out = work.path().join("copies");
    // Not allowed yet.
    match pull_once(&mut r, &out).await {
        Err(PullError::NotAllowed(_)) => {}
        other => panic!("expected NotAllowed, got {other:?}"),
    }
    assert!(!out.join("latest.db").exists(), "no copy before the owner allows it");
    // The owner allows it.
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: true }).await.unwrap(), ServerResponse::Replicas { .. }));
    let info = pull_once(&mut r, &out).await.expect("pull after being allowed");
    assert_eq!(info.history_id, sid, "the copy is of the server that was asked");
    assert!(info.events >= 3, "the signed history came along: {}", info.events);
    // The copy really holds the server's data, including what is NOT in the signed history.
    let copy = rusqlite::Connection::open(out.join("latest.db")).unwrap();
    let msgs: Vec<String> = copy.prepare("SELECT content FROM messages").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
    assert!(msgs.contains(&"history that must survive".to_string()), "{msgs:?}");
    let chans: Vec<String> = copy.prepare("SELECT name FROM channels").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
    assert!(chans.contains(&"general".to_string()) && chans.contains(&"only-in-the-database".to_string()), "channels live only in the database, not the signed history: {chans:?}");
    drop(copy);
    // A second pull keeps the first as previous.db.
    pull_once(&mut r, &out).await.unwrap();
    assert!(out.join("previous.db").exists() && out.join("latest.db").exists());
    // A damaged copy is rejected by the check (and a pull would have kept the old good one).
    let bad = work.path().join("bad.db");
    let mut bytes = std::fs::read(out.join("latest.db")).unwrap();
    for i in 0..64 { let at = bytes.len() / 2 + i; bytes[at] ^= 0xFF; }
    std::fs::write(&bad, &bytes).unwrap();
    assert!(verify_copy(&bad, bytes.len() as u64, "x").is_err(), "a corrupted copy must not verify");
    std::fs::write(&bad, b"not a database at all").unwrap();
    assert!(verify_copy(&bad, 21, "x").is_err());
    // The owner takes the permission away: no more copies.
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: false }).await.unwrap(), ServerResponse::Replicas { .. }));
    assert!(matches!(pull_once(&mut r, &out).await, Err(PullError::NotAllowed(_))));
}

/// A hash of every row of every table (not the file bytes: SQLite may touch its own header on open).
fn logical_hash(db: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([], |r| r.get(0)).unwrap().flatten().collect();
    let mut h = Sha256::new();
    for t in tables {
        h.update(t.as_bytes());
        let mut st = conn.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = st.column_count();
        let mut rows = st.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..n { h.update(format!("{:?}|", row.get_ref(i).unwrap()).as_bytes()); }
        }
    }
    hex::encode(h.finalize())
}

async fn start_read_only(copy: &std::path::Path) -> (std::process::Child, SocketAddr, tempfile::TempDir) {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/farder-server");
    let port = { let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); s.local_addr().unwrap().port() };
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("ro.log");
    let out = std::fs::File::create(&log).unwrap();
    let child = std::process::Command::new(&bin)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--read-only", "--db"]).arg(copy)
        .arg("--storage-dir").arg(dir.path().join("files")).arg("--data-dir").arg(dir.path().join("sd"))
        .stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if std::fs::read_to_string(&log).unwrap_or_default().contains("Server listening") { return (child, format!("127.0.0.1:{port}").parse().unwrap(), dir); }
    }
    panic!("read-only server did not start: {}", std::fs::read_to_string(&log).unwrap_or_default());
}

#[tokio::test]
async fn while_the_host_is_down_the_copy_serves_history_read_only_and_cannot_change() {
    let Seeded { srv, owner, mut o, sid, code } = seeded().await;
    let work = tempfile::tempdir().unwrap();
    let (rkey, _) = load_or_create_key(&work.path().join("replica.key")).unwrap();
    let mut r = Session::connect(srv.addr, None, &rkey, Some(code.clone()), None).await.unwrap();
    r.join_history(&rkey, &code).await.unwrap();
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: true }).await.unwrap(), ServerResponse::Replicas { .. }));
    let out = work.path().join("copies");
    pull_once(&mut r, &out).await.unwrap();
    // The host goes away.
    drop(o); drop(r);
    drop(srv);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let before = logical_hash(&out.join("latest.db"));
    let (mut child, addr, _d) = start_read_only(&out.join("latest.db")).await;
    // The owner, an existing member, signs in on the copy with the SAME key and reads the history.
    let mut c = Session::connect(addr, None, &owner, None, None).await.expect("an existing member can sign in to the copy");
    let ServerResponse::ServerInfo { name, server_id, channels, .. } = c.ask(ServerRequest::GetServerInfo).await.unwrap() else { panic!() };
    assert_eq!(server_id.as_deref(), Some(sid.as_str()), "it is the same server (same history id)");
    // Started without --name: the copy shows the name the host saved, so a friend's app that falls back to
    // it does not rename the server to the default.
    assert_eq!(name, "Replica Test", "the copy keeps the host's name");
    let general = channels.iter().find(|c| c.name == "general").unwrap().id;
    let ServerResponse::History { messages } = c.ask(ServerRequest::FetchHistory { channel_id: general, before_id: None, limit: 50 }).await.unwrap() else { panic!("history") };
    assert!(messages.iter().any(|m| m.content == "history that must survive"), "the saved history is readable");
    // Everything that writes is refused with the plain reason.
    for req in [
        ServerRequest::SendMessage { channel_id: general, content: "nope".into(), reply_to: None, attachment_ids: vec![] },
        ServerRequest::CreateChannel { name: "nope".into(), channel_type: ChannelType::Text, category_id: None, position: None },
        ServerRequest::CreateInvite { max_uses: None, expires_in_secs: None, target_channel: None },
    ] {
        let r = c.ask(req).await.unwrap();
        assert!(matches!(&r, ServerResponse::Error { reason } if reason.contains("saved copy")), "{r:?}");
    }
    // A newcomer cannot join a copy.
    let newcomer = Keypair::generate();
    let err = Session::connect(addr, None, &newcomer, Some("whatever".into()), None).await.err().expect("a newcomer is refused");
    assert!(format!("{err:#}").contains("saved copy"), "{err:#}");
    drop(c);
    let _ = child.kill(); let _ = child.wait();
    // The copy itself did not change: every row of every table is exactly as it was pulled.
    assert_eq!(logical_hash(&out.join("latest.db")), before, "a read-only copy must not change");
}

async fn read_upload(r: &mut quinn::RecvStream) -> UploadResponse {
    let mut len = [0u8; 4]; r.read_exact(&mut len).await.unwrap();
    let mut b = vec![0u8; u32::from_be_bytes(len) as usize]; r.read_exact(&mut b).await.unwrap();
    farder_protocol::codec::decode::<UploadResponse>(&b).unwrap()
}

/// Upload `data` as the session's member into `channel_id` (the app's upload stream). Returns the file id.
async fn upload(s: &Session, channel_id: u64, name: &str, data: &[u8]) -> u64 {
    use sha2::{Digest, Sha256};
    let (mut send, mut recv) = s.connection().open_bi().await.unwrap();
    let req = UploadRequest { channel_id, file_name: name.into(), file_size: data.len() as u64, hash: hex::encode(Sha256::digest(data)),
        mime_type: "text/plain".into(), width: None, height: None, duration_secs: None };
    let frame = |b: Vec<u8>| { let mut v = (b.len() as u32).to_be_bytes().to_vec(); v.extend(b); v };
    send.write_all(&frame(farder_protocol::codec::encode(&req).unwrap())).await.unwrap();
    match read_upload(&mut recv).await { UploadResponse::Ready => {}, other => panic!("upload: {other:?}") }
    send.write_all(data).await.unwrap();
    let _ = send.finish();
    match read_upload(&mut recv).await { UploadResponse::Complete { file_id } => file_id, other => panic!("upload: {other:?}") }
}

#[tokio::test]
async fn attachments_come_with_the_copy_are_checked_and_the_copy_serves_them_while_the_host_is_down() {
    let Seeded { srv, owner, mut o, sid: _, code } = seeded().await;
    let ServerResponse::ServerInfo { channels, .. } = o.ask(ServerRequest::GetServerInfo).await.unwrap() else { panic!() };
    let general = channels.iter().find(|c| c.name == "general").unwrap().id;
    // One file on a message in #general, and one the owner uploaded but never posted (no channel can see it).
    let posted = upload(&o, general, "posted.txt", b"an attachment that must survive").await;
    assert!(matches!(o.ask(ServerRequest::SendMessage { channel_id: general, content: "see file".into(), reply_to: None, attachment_ids: vec![posted] }).await.unwrap(), ServerResponse::MessageSent { .. }));
    let unposted = upload(&o, general, "owner-only.txt", b"only the owner and a replica may fetch this").await;

    let work = tempfile::tempdir().unwrap();
    let (rkey, _) = load_or_create_key(&work.path().join("replica.key")).unwrap();
    let mut r = Session::connect(srv.addr, None, &rkey, Some(code.clone()), None).await.unwrap();
    r.join_history(&rkey, &code).await.unwrap();
    // As a plain member it can fetch the posted file but not the unposted one.
    assert!(r.download(posted).await.unwrap().is_some(), "a member who can read #general may fetch its file");
    assert!(r.download(unposted).await.unwrap().is_none(), "a plain member may not fetch a file no channel shows");
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: true }).await.unwrap(), ServerResponse::Replicas { .. }));
    let out = work.path().join("copies");
    pull_once(&mut r, &out).await.unwrap();
    let f = sync_files(&mut r, &out).await.unwrap();
    assert_eq!(f, FileSync { fetched: 2, already_had: 0, removed: 0, unavailable: 0 }, "an allowed replica takes every file");
    // A second sync downloads nothing again.
    assert_eq!(sync_files(&mut r, &out).await.unwrap(), FileSync { fetched: 0, already_had: 2, removed: 0, unavailable: 0 });
    // A file that was tampered with on the copy's disk is fetched again (the hash is checked, not just the name).
    let posted_hash: String = rusqlite::Connection::open(out.join("latest.db")).unwrap()
        .query_row("SELECT hash FROM files WHERE id = ?1", [posted as i64], |r| r.get(0)).unwrap();
    let posted_path = farder_server::attachments::content_path(&out.join("files").to_string_lossy(), &posted_hash);
    std::fs::write(&posted_path, b"tampered").unwrap();
    assert_eq!(sync_files(&mut r, &out).await.unwrap().fetched, 1);
    assert_eq!(std::fs::read(&posted_path).unwrap(), b"an attachment that must survive");

    // Revoking the replica takes its file access away again (the unposted file is owner-only once more).
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: false }).await.unwrap(), ServerResponse::Replicas { .. }));
    assert!(r.download(unposted).await.unwrap().is_none(), "a revoked replica may not fetch an owner-only file");
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: true }).await.unwrap(), ServerResponse::Replicas { .. }));

    // The host goes away; the copy serves the file to an existing member.
    drop(o); drop(r); drop(srv);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (mut child, addr, _d) = start_read_only_with_files(&out.join("latest.db"), &out.join("files")).await;
    let mut c = Session::connect(addr, None, &owner, None, None).await.expect("the owner signs in to the copy");
    let (_, bytes) = c.download(posted).await.unwrap().expect("the copy serves the attachment");
    assert_eq!(bytes, b"an attachment that must survive");
    drop(c);
    let _ = child.kill(); let _ = child.wait();

    // A file the host redacted (marked in the next copy) is deleted from the copy's disk.
    rusqlite::Connection::open(out.join("latest.db")).unwrap()
        .execute("UPDATE files SET redacted_by = x'00' WHERE id = ?1", [posted as i64]).unwrap();
    let srv2 = real_server().await;      // any reachable server: nothing is fetched, only removed
    let k = Keypair::generate();
    let mut s2 = Session::connect(srv2.addr, None, &k, None, Some(srv2.token.clone())).await.unwrap();
    let f = sync_files(&mut s2, &out).await.unwrap();
    assert_eq!((f.removed, f.already_had), (1, 1), "{f:?}");
    assert!(!posted_path.exists(), "a redacted file must not stay on the copy");
}

async fn start_read_only_with_files(copy: &std::path::Path, files: &std::path::Path) -> (std::process::Child, SocketAddr, tempfile::TempDir) {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/farder-server");
    let port = { let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); s.local_addr().unwrap().port() };
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("ro.log");
    let out = std::fs::File::create(&log).unwrap();
    let child = std::process::Command::new(&bin)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--read-only", "--db"]).arg(copy)
        .arg("--storage-dir").arg(files).arg("--data-dir").arg(dir.path().join("sd"))
        .stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if std::fs::read_to_string(&log).unwrap_or_default().contains("Server listening") { return (child, format!("127.0.0.1:{port}").parse().unwrap(), dir); }
    }
    panic!("read-only server did not start: {}", std::fs::read_to_string(&log).unwrap_or_default());
}

// ---------------------------------------------------------------------------------------------
// Promotion, step 1: an owner-signed HostTerm turns a read-only copy into the writable host.
// ---------------------------------------------------------------------------------------------

/// Start the real server binary on `db` with an explicit `--data-dir` (its host identity lives there).
async fn start_at(db: &std::path::Path, data: &std::path::Path, read_only: bool) -> (std::process::Child, SocketAddr, std::path::PathBuf) {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/farder-server");
    let port = { let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); s.local_addr().unwrap().port() };
    let logdir = tempfile::tempdir().unwrap().keep();
    let log = logdir.join("srv.log");
    let out = std::fs::File::create(&log).unwrap();
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(["--bind", &format!("127.0.0.1:{port}"), "--db"]).arg(db)
        .arg("--storage-dir").arg(logdir.join("files")).arg("--data-dir").arg(data);
    if read_only { cmd.arg("--read-only"); }
    let child = cmd.stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if std::fs::read_to_string(&log).unwrap_or_default().contains("Server listening") { return (child, format!("127.0.0.1:{port}").parse().unwrap(), log); }
    }
    panic!("server did not start: {}", std::fs::read_to_string(&log).unwrap_or_default());
}

fn head_of(db: &std::path::Path) -> String {
    rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
        .query_row("SELECT event_hash FROM events ORDER BY accept_seq DESC LIMIT 1", [], |r| r.get(0)).unwrap()
}

fn identity_of(data: &std::path::Path) -> [u8; 32] {
    std::fs::read(data.join("server_id")).unwrap().try_into().expect("a 32-byte host identity")
}

fn now_secs() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() }

async fn refused(s: &mut Session, term: farder_crypto::host_term::HostTerm, expect: &str) {
    match s.ask(ServerRequest::PromoteToHost { term }).await.unwrap() {
        ServerResponse::Error { reason } => assert!(reason.contains(expect), "wanted a reason containing {expect:?}, got {reason:?}"),
        other => panic!("the term must be refused ({expect}), got {other:?}"),
    }
}

async fn still_read_only(s: &mut Session, general: u64) {
    let r = s.ask(ServerRequest::SendMessage { channel_id: general, content: "must not be accepted".into(), reply_to: None, attachment_ids: vec![] }).await.unwrap();
    assert!(matches!(&r, ServerResponse::Error { reason } if reason.contains("saved copy")), "the copy must stay read-only: {r:?}");
}

#[tokio::test]
async fn the_owner_promotes_a_read_only_copy_to_the_writable_host_and_it_stays_promoted_after_a_restart() {
    use farder_crypto::host_term::HostTerm;
    let Seeded { srv, owner, mut o, sid, code } = seeded().await;
    let host_data = srv._dir.path().join("sd");
    let work = tempfile::tempdir().unwrap();
    let (rkey, _) = load_or_create_key(&work.path().join("replica.key")).unwrap();
    let mut r = Session::connect(srv.addr, None, &rkey, Some(code.clone()), None).await.unwrap();
    r.join_history(&rkey, &code).await.unwrap();
    assert!(matches!(o.ask(ServerRequest::SetReplica { member: rkey.public_key(), enabled: true }).await.unwrap(), ServerResponse::Replicas { .. }));
    let out = work.path().join("copies");
    pull_once(&mut r, &out).await.unwrap();
    let db = out.join("latest.db");
    let ServerResponse::ServerInfo { channels, .. } = o.ask(ServerRequest::GetServerInfo).await.unwrap() else { panic!() };
    let general = channels.iter().find(|c| c.name == "general").unwrap().id;

    // The original host: it holds no term, and it REFUSES promotion (a valid, owner-signed term does not matter).
    assert!(matches!(o.ask(ServerRequest::GetHostTerm).await.unwrap(), ServerResponse::HostTerm { term: None }));
    let on_host = HostTerm::sign(&owner, sid.clone(), 1, identity_of(&host_data), vec![], head_of(&db), now_secs());
    refused(&mut o, on_host, "already the writable host").await;
    assert!(matches!(o.ask(ServerRequest::SendMessage { channel_id: general, content: "the host still writes".into(), reply_to: None, attachment_ids: vec![] }).await.unwrap(), ServerResponse::MessageSent { .. }));
    // (that message is newer than the copy's head: the copy stays as pulled)
    drop(o); drop(r); drop(srv);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The copy, read-only, with its own data dir (= its host identity).
    let data = work.path().join("copy-data");
    let (mut child, addr, _log) = start_at(&db, &data, true).await;
    let me = identity_of(&data);
    let mut c = Session::connect(addr, None, &owner, None, None).await.unwrap();
    let mut m = Session::connect(addr, None, &rkey, None, None).await.unwrap();
    assert!(matches!(m.ask(ServerRequest::GetHostTerm).await.unwrap(), ServerResponse::HostTerm { term: None }), "a copy answers GetHostTerm");
    still_read_only(&mut m, general).await;

    // What the owner's app asks the copy before signing: its key, its head, its term, read-only.
    let ServerResponse::PromotionInfo { host_key, head_event, term, read_only } = c.ask(ServerRequest::GetPromotionInfo).await.unwrap() else { panic!("GetPromotionInfo") };
    assert_eq!(host_key, hex::encode(me), "the copy reports its own host key");
    assert_eq!(head_event, head_of(&db), "and the head of its log");
    assert_eq!((term, read_only), (0, true));

    let head = head_event;
    let good = |term: u64| HostTerm::sign(&owner, sid.clone(), term, me, vec!["127.0.0.1:1".into()], head.clone(), now_secs());
    // Each of these is refused, and the copy stays read-only.
    let stranger = Keypair::generate();
    refused(&mut c, HostTerm::sign(&stranger, sid.clone(), 1, me, vec![], head.clone(), now_secs()), "not signed by the owner").await;
    // The replica's own key is a member key, not the owner: also refused.
    refused(&mut c, HostTerm::sign(&rkey, sid.clone(), 1, me, vec![], head.clone(), now_secs()), "not signed by the owner").await;
    refused(&mut c, HostTerm::sign(&owner, "someone-else".into(), 1, me, vec![], head.clone(), now_secs()), "different server").await;
    refused(&mut c, HostTerm::sign(&owner, sid.clone(), 1, [9u8; 32], vec![], head.clone(), now_secs()), "different machine").await;
    refused(&mut c, HostTerm::sign(&owner, sid.clone(), 1, me, vec![], "not-the-head".into(), now_secs()), "not the head").await;
    refused(&mut c, HostTerm::sign(&owner, sid.clone(), 0, me, vec![], head.clone(), now_secs()), "not signed by the owner").await;
    let mut tampered = good(1); tampered.after_event = head.clone(); tampered.term = 5;
    refused(&mut c, tampered, "not signed by the owner").await;
    still_read_only(&mut m, general).await;
    assert!(matches!(m.ask(ServerRequest::GetHostTerm).await.unwrap(), ServerResponse::HostTerm { term: None }));

    // The real promotion.
    let t1 = good(1);
    assert!(matches!(c.ask(ServerRequest::PromoteToHost { term: t1.clone() }).await.unwrap(), ServerResponse::Ok));
    // No restart: the SAME connection of a member now writes.
    let sent = m.ask(ServerRequest::SendMessage { channel_id: general, content: "written after promotion".into(), reply_to: None, attachment_ids: vec![] }).await.unwrap();
    assert!(matches!(sent, ServerResponse::MessageSent { .. }), "the promoted copy accepts a message: {sent:?}");
    let ServerResponse::HostTerm { term: Some(held) } = m.ask(ServerRequest::GetHostTerm).await.unwrap() else { panic!("GetHostTerm must return the term") };
    assert_eq!(held, t1);
    let ServerResponse::PromotionInfo { term, read_only, .. } = m.ask(ServerRequest::GetPromotionInfo).await.unwrap() else { panic!() };
    assert_eq!((term, read_only), (1, false), "after promotion: term 1, writable");
    // Now it is a writable host: it refuses a further promotion.
    refused(&mut c, good(2), "already the writable host").await;
    drop(c); drop(m);
    let _ = child.kill(); let _ = child.wait();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Restart from the same --db, WITHOUT --read-only: still writable, same term.
    let (mut child, addr, _log) = start_at(&db, &data, false).await;
    let mut m = Session::connect(addr, None, &rkey, None, None).await.unwrap();
    let ServerResponse::HostTerm { term: Some(held) } = m.ask(ServerRequest::GetHostTerm).await.unwrap() else { panic!("the term must survive a restart") };
    assert_eq!(held, t1, "same term after restart");
    let sent = m.ask(ServerRequest::SendMessage { channel_id: general, content: "written after restart".into(), reply_to: None, attachment_ids: vec![] }).await.unwrap();
    assert!(matches!(sent, ServerResponse::MessageSent { .. }), "still writable after restart: {sent:?}");
    drop(m);
    let _ = child.kill(); let _ = child.wait();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The same database on a DIFFERENT machine (another identity) must not write, even with no flag:
    // its stored term names someone else as the host.
    let (mut child, addr, _log) = start_at(&db, &work.path().join("other-machine"), false).await;
    let mut m = Session::connect(addr, None, &rkey, None, None).await.unwrap();
    still_read_only(&mut m, general).await;
    drop(m);
    let _ = child.kill(); let _ = child.wait();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Explicitly read-only again with a stored term: a term that is not higher is refused (even a valid one).
    let (mut child, addr, _log) = start_at(&db, &data, true).await;
    let mut c = Session::connect(addr, None, &owner, None, None).await.unwrap();
    let mut m = Session::connect(addr, None, &rkey, None, None).await.unwrap();
    let head2 = head_of(&db);
    refused(&mut c, HostTerm::sign(&owner, sid.clone(), 1, me, vec![], head2.clone(), now_secs()), "not higher").await;
    still_read_only(&mut m, general).await;
    assert!(matches!(m.ask(ServerRequest::GetHostTerm).await.unwrap(), ServerResponse::HostTerm { term: Some(t) } if t == t1), "the stored term is unchanged");
    let _ = child.kill(); let _ = child.wait();
}
