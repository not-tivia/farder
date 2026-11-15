//! A replica against the REAL farder-server binary: it joins with its own key, is refused until the
//! owner allows it, then pulls a verified copy that really contains the server's data.
use farder_crypto::event_log::{invite_code_hash, DeviceCert, Event, EventPayload};
use farder_crypto::identity::Keypair;
use farder_protocol::server::*;
use farder_replica::{load_or_create_key, pull_once, verify_copy, PullError, Session};
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
    let ServerResponse::ServerInfo { server_id, channels, .. } = c.ask(ServerRequest::GetServerInfo).await.unwrap() else { panic!() };
    assert_eq!(server_id.as_deref(), Some(sid.as_str()), "it is the same server (same history id)");
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
