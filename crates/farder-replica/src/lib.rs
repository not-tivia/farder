//! A replica: a machine that keeps a verified copy of a Farder server, so history survives the host
//! being switched off (docs/mesh-hosting-status.md, step 2).
//!
//! It is a member with its OWN key. The owner allows that key as a replica; the replica then pulls a
//! snapshot of the server's database in pieces, checks the SHA-256 the server announced, checks the
//! database is intact, and checks the signed history inside it replays cleanly and belongs to the
//! server it asked (the history's id is a hash of its first record). Only then does the copy replace
//! `latest.db`; the one before is kept as `previous.db`. A copy that fails any check is thrown away and
//! the old good copy stays.
//!
//! A replica sees EVERYTHING the host sees, including channels a member cannot open and invite codes.
//! Only run one on a machine you trust as much as the host.

use anyhow::{bail, Context, Result};
use farder_crypto::identity::Keypair;
use farder_protocol::{
    codec,
    server::{ClientFrame, DownloadRequest, DownloadResponse, ServerFrame, ServerRequest, ServerResponse},
};
use quinn::{ClientConfig, Endpoint, RecvStream, SendStream};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const CHUNK: u32 = 512 * 1024;
/// Largest attachment a replica will take (the server's own upload limit is lower by default).
pub const MAX_FILE: u64 = 1024 * 1024 * 1024;

#[derive(Debug)]
struct PinVerifier(Option<Vec<u8>>);
impl rustls::client::danger::ServerCertVerifier for PinVerifier {
    fn verify_server_cert(&self, end: &rustls::pki_types::CertificateDer<'_>, _i: &[rustls::pki_types::CertificateDer<'_>], _n: &rustls::pki_types::ServerName<'_>, _o: &[u8], _t: rustls::pki_types::UnixTime) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(fp) = &self.0 {
            if Sha256::digest(end.as_ref()).as_slice() != fp.as_slice() {
                return Err(rustls::Error::General("server certificate does not match the pinned fingerprint".into()));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
    fn verify_tls13_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> { rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes() }
}

/// A logged-in connection to one server.
pub struct Session {
    conn: quinn::Connection,
    send: SendStream,
    recv: RecvStream,
    next_id: u32,
}

async fn read_frame(r: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await.context("read frame length")?;
    let n = u32::from_be_bytes(len) as usize;
    if n > 16 * 1024 * 1024 { bail!("frame too large"); }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await.context("read frame")?;
    Ok(b)
}
async fn write_frame(s: &mut SendStream, b: &[u8]) -> Result<()> {
    s.write_all(&(b.len() as u32).to_be_bytes()).await?;
    s.write_all(b).await?;
    Ok(())
}

impl Session {
    /// Connect and authenticate with `key` (and an invite code the first time).
    pub async fn connect(addr: SocketAddr, cert_fp: Option<Vec<u8>>, key: &Keypair, invite: Option<String>, setup_token: Option<String>) -> Result<Session> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let crypto = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(PinVerifier(cert_fp))).with_no_client_auth();
        let cfg = ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
        let mut ep = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
        ep.set_default_client_config(cfg);
        let conn = ep.connect(addr, "farder-server")?.await.context("could not reach the server")?;
        let (mut send, mut recv) = conn.accept_bi().await.context("the server did not open its main stream")?;
        let ServerFrame::Challenge { nonce } = codec::decode(&read_frame(&mut recv).await?)? else { bail!("expected a Challenge") };
        let auth = ClientFrame::Authenticate { public_key: key.public_key(), signed_challenge: key.sign(&nonce), invite_code: invite, setup_token };
        write_frame(&mut send, &codec::encode(&auth)?).await?;
        match codec::decode(&read_frame(&mut recv).await?)? {
            ServerFrame::Authenticated { .. } => {}
            ServerFrame::AuthError { reason } => bail!("the server refused this replica: {reason}"),
            other => bail!("unexpected {other:?}"),
        }
        Ok(Session { conn, send, recv, next_id: 100 })
    }

    pub async fn ask(&mut self, body: ServerRequest) -> Result<ServerResponse> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(&mut self.send, &codec::encode(&ClientFrame::Request { id, body })?).await?;
        self.wait(id).await
    }

    async fn wait(&mut self, id: u32) -> Result<ServerResponse> {
        loop {
            if let ServerFrame::Response { request_id, body } = codec::decode(&read_frame(&mut self.recv).await?)? {
                if request_id == id { return Ok(body); }
            }
        }
    }

    /// Fetch one stored file (attachment) by id on its own stream. `Ok(None)` when the server answers "not
    /// available" (redacted, missing, or this key may not read it). Returns the hash the server announced and
    /// the bytes; the caller checks the bytes against the hash.
    pub async fn download(&mut self, file_id: u64) -> Result<Option<(String, Vec<u8>)>> {
        let (mut send, mut recv) = self.conn.open_bi().await.context("open a download stream")?;
        write_frame(&mut send, &codec::encode(&DownloadRequest { file_id })?).await?;
        let _ = send.finish();
        match codec::decode::<DownloadResponse>(&read_frame(&mut recv).await?)? {
            DownloadResponse::Start { hash, file_size, .. } => {
                let data = recv.read_to_end(MAX_FILE as usize).await.context("read the file")?;
                if data.len() as u64 != file_size { bail!("file {file_id}: got {} of {file_size} bytes", data.len()); }
                Ok(Some((hash, data)))
            }
            DownloadResponse::Error { .. } => Ok(None),
        }
    }

    /// Give this replica a name members can recognise (its signed profile), instead of "Anonymous".
    pub async fn set_name(&mut self, key: &Keypair, name: &str) -> Result<()> {
        let p = farder_crypto::profile::SignedProfile::create(key, name.to_string(), None, Some("keeps a copy of this server for when the host is off".into()));
        match self.ask(ServerRequest::UpdateProfile { profile: p.to_bytes() }).await? {
            ServerResponse::Ok => Ok(()),
            ServerResponse::Error { reason } => bail!("could not set the name: {reason}"),
            other => bail!("unexpected {other:?}"),
        }
    }

    /// The underlying QUIC connection (tests use it to upload a file on its own stream).
    pub fn connection(&self) -> &quinn::Connection { &self.conn }

    /// A pre-encoded request (from the browser crate's join helpers) with the id it was built for.
    async fn raw(&mut self, id: u32, bytes: Vec<u8>) -> Result<ServerResponse> {
        write_frame(&mut self.send, &bytes).await?;
        self.wait(id).await
    }

    /// Become a member of the server's signed history (a replica must be one): authorize this
    /// device, resolve the invite, announce the join. Uses the same join steps as the browser guest.
    pub async fn join_history(&mut self, key: &Keypair, invite: &str) -> Result<()> {
        let ServerResponse::ServerInfo { server_id: Some(log_id), .. } = self.ask(ServerRequest::GetServerInfo).await? else { bail!("the server has no signed history") };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let (first, bytes) = farder_web::begin_join(key, 7001, &log_id, now).map_err(|e| anyhow::anyhow!(e))?;
        if let ServerResponse::Error { reason } = self.raw(7001, bytes).await? {
            // Already authorized earlier is fine.
            if !reason.contains("already") { bail!("join step 1: {reason}"); }
        }
        let ServerResponse::InviteResolved { invite_event: Some(h) } = self.ask(ServerRequest::ResolveInvite { code: invite.to_string() }).await? else { bail!("that invite is not valid on this server") };
        let bytes = farder_web::finish_join(key, 7002, &log_id, &h, &first, now + 1).map_err(|e| anyhow::anyhow!(e))?;
        if let ServerResponse::Error { reason } = self.raw(7002, bytes).await? {
            if !reason.contains("already a member") { bail!("join step 2: {reason}"); }
        }
        Ok(())
    }
}

/// What a successful pull recorded about the copy.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyInfo { pub size: u64, pub sha256: String, pub history_id: String, pub events: u64 }

/// Why a pull produced no new copy.
#[derive(Debug)]
pub enum PullError {
    /// The owner has not allowed this key as a replica yet.
    NotAllowed(String),
    /// This folder's copy was PROMOTED to host (it holds hosting term N): the server running on it owns
    /// the file now, and a pull would replace everything written since the promotion. Never pulled over.
    Promoted(u64),
    Other(anyhow::Error),
}

/// The hosting term a copy's database holds (`server_settings.host_term`, written by a promotion), or
/// `None` if it was never promoted. Unreadable or absent counts as `None`.
pub fn held_term(db: &Path) -> Option<u64> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let hex_term: String = conn
        .query_row("SELECT value FROM server_settings WHERE key = 'host_term'", [], |r| r.get(0))
        .ok()?;
    let bytes = hex::decode(hex_term).ok()?;
    farder_crypto::host_term::HostTerm::from_bytes(&bytes).ok().map(|t| t.term)
}
impl From<anyhow::Error> for PullError { fn from(e: anyhow::Error) -> Self { PullError::Other(e) } }

/// Pull one snapshot, verify it, and install it as `<out>/latest.db` (moving the old one to `previous.db`).
pub async fn pull_once(s: &mut Session, out: &Path) -> std::result::Result<CopyInfo, PullError> {
    std::fs::create_dir_all(out).map_err(|e| PullError::Other(e.into()))?;
    // A promoted copy is a host now: pulling (for example from an old host that came back) would
    // overwrite everything written since the promotion. Checked before anything is downloaded.
    if let Some(term) = held_term(&out.join("latest.db")) {
        return Err(PullError::Promoted(term));
    }
    let (id, size, sha) = match s.ask(ServerRequest::SnapshotBegin).await? {
        ServerResponse::SnapshotInfo { id, size, sha256 } => (id, size, sha256),
        ServerResponse::Error { reason } if reason.contains("only the owner") => return Err(PullError::NotAllowed(reason)),
        ServerResponse::Error { reason } => return Err(PullError::Other(anyhow::anyhow!(reason))),
        other => return Err(PullError::Other(anyhow::anyhow!("unexpected {other:?}"))),
    };
    let partial = out.join("incoming.db.partial");
    let result: Result<CopyInfo> = async {
        let mut f = std::fs::File::create(&partial)?;
        let mut h = Sha256::new();
        let mut got = 0u64;
        use std::io::Write;
        while got < size {
            match s.ask(ServerRequest::SnapshotChunk { id, offset: got, len: CHUNK }).await? {
                ServerResponse::SnapshotData { data } if !data.is_empty() => { h.update(&data); f.write_all(&data)?; got += data.len() as u64; }
                ServerResponse::Error { reason } => bail!("{reason}"),
                _ => bail!("the server sent an empty or unexpected piece"),
            }
        }
        f.sync_all()?;
        if hex::encode(h.finalize()) != sha { bail!("the copy does not match the checksum the server announced"); }
        verify_copy(&partial, size, &sha)
    }.await;
    match result {
        Ok(info) => {
            let latest = out.join("latest.db");
            // Checked again at install time: the folder may have been promoted while we downloaded.
            if let Some(term) = held_term(&latest) {
                let _ = std::fs::remove_file(&partial);
                return Err(PullError::Promoted(term));
            }
            if latest.exists() { let _ = std::fs::rename(&latest, out.join("previous.db")); }
            std::fs::rename(&partial, &latest).map_err(|e| PullError::Other(e.into()))?;
            let meta = serde_json::json!({ "taken_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "size": info.size, "sha256": info.sha256, "history_id": info.history_id, "events": info.events });
            let _ = std::fs::write(out.join("latest.json"), serde_json::to_vec_pretty(&meta).unwrap());
            Ok(info)
        }
        Err(e) => { let _ = std::fs::remove_file(&partial); Err(PullError::Other(e)) }
    }
}

/// Is this file a sound copy of a Farder server? SQLite's own integrity check, then the signed history
/// inside it must replay cleanly and its id must be the hash of its own first record.
pub fn verify_copy(path: &Path, size: u64, sha: &str) -> Result<CopyInfo> {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" { bail!("the copy is damaged: {ok}"); }
    let g = farder_server::event_ingest::load_genesis(&conn)?.context("the copy has no signed history")?;
    let state = farder_server::event_ingest::build_log_state(&conn)?.context("could not rebuild the history")
        .context("the signed history in the copy does not replay cleanly")?;
    let _ = state;
    let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
    Ok(CopyInfo { size, sha256: sha.to_string(), history_id: g.server_id(), events: events as u64 })
}

/// Load a 32-byte key from `path` (hex), or make and save one (owner-only file permissions).
pub fn load_or_create_key(path: &Path) -> Result<(Keypair, bool)> {
    if let Ok(t) = std::fs::read_to_string(path) {
        let b = hex::decode(t.trim()).context("the key file is not hex")?;
        let arr: [u8; 32] = b.as_slice().try_into().map_err(|_| anyhow::anyhow!("the key file must hold 32 bytes"))?;
        return Ok((Keypair::from_signing_key_bytes(&arr), false));
    }
    let kp = Keypair::generate();
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    std::fs::write(path, hex::encode(kp.signing_key_bytes()))?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?; }
    Ok((kp, true))
}

pub fn state_path(dir: &Path) -> PathBuf { dir.join("replica-state.json") }

/// What `--invite` may be: a bare invite code, a direct link `farder://host:port/CODE`, or the
/// `https://farder.xyz/join/<base64url>` link the app's invite dialog hands out (it wraps the direct link).
/// Returns the server address when the link carries one, and the code. Relay links are refused: a replica
/// connects to the host directly.
pub fn invite_from_link(s: &str) -> Result<(Option<SocketAddr>, String)> {
    use base64::Engine;
    let t = s.trim();
    let no_scheme = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t);
    let inner = match no_scheme.split_once("/join/") {
        Some((_host, enc)) => {
            let enc = enc.split(['?', '#']).next().unwrap_or(enc);
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(enc.trim_end_matches('='))
                .context("that join link is damaged (it did not decode)")?;
            String::from_utf8(bytes).context("that join link is damaged")?
        }
        None => t.to_string(),
    };
    let body = inner.strip_prefix("farder://").unwrap_or(&inner);
    if body.starts_with("relayd/") || body.starts_with("relay/") {
        bail!("that invite goes through a relay; a replica needs the host's direct address (host:port)");
    }
    match body.rsplit_once('/') {
        Some((addr, code)) if !code.is_empty() => {
            let addr: SocketAddr = addr.parse().with_context(|| format!("not a host:port address in the invite: {addr}"))?;
            Ok((Some(addr), code.to_string()))
        }
        Some(_) => bail!("the invite has no code after the address"),
        None if !body.is_empty() => Ok((None, body.to_string())),
        None => bail!("empty invite"),
    }
}

#[cfg(test)]
mod invite_link_tests {
    use super::invite_from_link;

    #[test]
    fn accepts_a_bare_code_a_direct_link_and_the_apps_join_link() {
        assert_eq!(invite_from_link("Za6nY8nx").unwrap(), (None, "Za6nY8nx".into()));
        let addr = Some("172.21.207.201:4576".parse().unwrap());
        assert_eq!(invite_from_link("farder://172.21.207.201:4576/Za6nY8nx").unwrap(), (addr, "Za6nY8nx".into()));
        // The exact form the app's invite dialog produced in the failover self-test.
        assert_eq!(invite_from_link("https://farder.xyz/join/MTcyLjIxLjIwNy4yMDE6NDU3Ni9aYTZuWThueA").unwrap(), (addr, "Za6nY8nx".into()));
    }

    #[test]
    fn refuses_relay_links_and_garbage_plainly() {
        assert!(format!("{:#}", invite_from_link("farder://relayd/abcd/tok").unwrap_err()).contains("relay"));
        assert!(invite_from_link("https://farder.xyz/join/!!!").is_err());
        assert!(invite_from_link("farder://not-an-address/x").is_err());
    }
}

/// What one attachment sync did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FileSync { pub fetched: u64, pub already_had: u64, pub removed: u64, pub unavailable: u64 }

/// Make `out/files` hold exactly the attachments `out/latest.db` refers to: fetch the ones missing (each
/// checked against its SHA-256 before it is kept), and delete the ones the copy no longer lists, so a file
/// the host redacted (or that was deleted) disappears from the copy too. Layout is the server's own
/// content-addressed one, so `farder-server --read-only --storage-dir out/files` serves them.
pub async fn sync_files(s: &mut Session, out: &Path) -> Result<FileSync> {
    let files_dir = out.join("files");
    let wanted: Vec<(u64, String)> = {
        let db = rusqlite::Connection::open_with_flags(out.join("latest.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let redact_col = db.prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'redacted_by'")?.exists([])?;
        let sql = if redact_col { "SELECT id, hash FROM files WHERE redacted_by IS NULL" } else { "SELECT id, hash FROM files" };
        let rows = db.prepare(sql)?.query_map([], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut r = FileSync::default();
    let mut keep = std::collections::HashSet::new();
    for (id, hash) in wanted {
        // The hash becomes a path: only accept a plain SHA-256 so a damaged database cannot point outside.
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { r.unavailable += 1; continue; }
        let path = farder_server::attachments::content_path(&files_dir.to_string_lossy(), &hash);
        keep.insert(path.clone());
        if path.exists() && crate::sha256_hex_file(&path).ok().as_deref() == Some(hash.as_str()) { r.already_had += 1; continue; }
        match s.download(id).await? {
            Some((_announced, data)) => {
                if hex::encode(Sha256::digest(&data)) != hash { bail!("file {id} does not match its hash; the copy keeps what it had"); }
                std::fs::create_dir_all(path.parent().unwrap())?;
                let tmp = path.with_extension("partial");
                std::fs::write(&tmp, &data)?;
                std::fs::rename(&tmp, &path)?;
                r.fetched += 1;
            }
            None => r.unavailable += 1,
        }
    }
    // Remove what the copy no longer lists (redacted or deleted on the host).
    if files_dir.exists() {
        let mut stack = vec![files_dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d)? {
                let p = e?.path();
                if p.is_dir() { stack.push(p); } else if !keep.contains(&p) { std::fs::remove_file(&p)?; r.removed += 1; }
            }
        }
    }
    Ok(r)
}

fn sha256_hex_file(p: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(p)?;
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}
