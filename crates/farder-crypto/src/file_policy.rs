//! Client-side file safety for E2EE attachments (spec sub-project 6, I6).
//!
//! # Why this exists, and why it lives here
//!
//! In a plaintext channel the SERVER inspects uploaded bytes: magic-byte
//! sniffing, a content-type allowlist, filename sanitization on download. For an
//! encrypted channel it cannot — it holds ciphertext and a neutral
//! `application/octet-stream` label. **The entire server-side file-hardening
//! track is inoperative for sealed blobs**, which makes E2EE channels the bypass
//! around every server-side content control the project has.
//!
//! The real filename and MIME travel *inside* the message ciphertext, so they are
//! fully attacker-controlled and no server sanitizer ever sees them. Everything
//! below is the client-side replacement, and it is the only thing standing
//! between a hostile sender and a path-traversal write on the recipient's disk.
//!
//! It lives in `farder-crypto` because both the server and the client depend on
//! this crate (as `media.rs` already does for the same reason). The spec requires
//! the client to sniff against *the same allowlist the server would have
//! applied*; two copies of an allowlist drift, and a drifted allowlist is a
//! bypass.
//!
//! # The discipline
//!
//! - **Sanitize on the way OUT** — immediately before a write or a render, never
//!   on receipt. A sanitized value stored anywhere becomes a value something
//!   later trusts.
//! - **Fail closed and say why.** A name that cannot be made safe, or bytes that
//!   contradict their claimed type, are refused — not quietly "cleaned". Silent
//!   cleaning hides an attack from the person it targets.

use std::fmt;

/// Why a filename or a file's bytes were refused. Each variant names one rule,
/// so the UI can say what happened and tests can assert the SPECIFIC refusal
/// rather than "some error".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRefused {
    /// The name contained a path separator or a traversal component.
    PathInName,
    /// Nothing usable survived sanitization (empty, all dots, all stripped).
    EmptyName,
    /// The name is longer than [`MAX_FILENAME_LEN`] after sanitization.
    NameTooLong,
    /// The extension is not on the allowlist.
    ExtensionNotAllowed(String),
    /// The bytes' real format contradicts the claimed one (or is unknown).
    ContentMismatch { claimed: String, sniffed: Option<String> },
}

impl fmt::Display for FileRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathInName => write!(f, "the file name contains a folder path"),
            Self::EmptyName => write!(f, "the file name is empty"),
            Self::NameTooLong => write!(f, "the file name is too long"),
            Self::ExtensionNotAllowed(ext) => {
                write!(f, "files of type .{ext} are not allowed")
            }
            Self::ContentMismatch { claimed, sniffed } => match sniffed {
                Some(s) => write!(f, "the file says it is {claimed} but its contents are {s}"),
                None => write!(f, "the file says it is {claimed} but its contents are unrecognized"),
            },
        }
    }
}

impl std::error::Error for FileRefused {}

/// Maximum sanitized filename length. Long names are a real hazard (path-length
/// limits, UI truncation hiding a double extension), not a style preference.
pub const MAX_FILENAME_LEN: usize = 120;

/// The extensions a sealed attachment may carry. Deliberately a SHORT allowlist,
/// not a denylist: a denylist is a promise to have thought of every dangerous
/// extension, which nobody can keep.
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", // images
    "wav", "mp3", "ogg", "m4a", "flac", "webm", // audio (incl. voice messages)
    "mp4", "mov", // video
    "pdf", "txt", "md", "csv", "json", "log", // documents
    "zip", // archives
];

/// A format recognized from a file's leading bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SniffedType {
    /// The canonical MIME for the sniffed format.
    pub mime: &'static str,
}

/// Identify a file's real format from its magic bytes.
///
/// Returns `None` for anything not recognized — which is a refusal, not a
/// shrug: an unrecognized attachment does not get written to disk.
pub fn sniff(bytes: &[u8]) -> Option<SniffedType> {
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        "audio/wav"
    } else if bytes.starts_with(b"OggS") {
        "audio/ogg"
    } else if bytes.starts_with(b"fLaC") {
        "audio/flac"
    } else if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        // Matroska/WebM container — audio or video.
        "video/webm"
    } else if bytes.starts_with(b"ID3") || bytes.starts_with(&[0xFF, 0xFB]) {
        "audio/mpeg"
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        "video/mp4"
    } else if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else if bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]) {
        "application/zip"
    } else {
        return None;
    };
    Some(SniffedType { mime })
}

/// Make an attacker-supplied filename safe to write or display, or refuse it.
///
/// Applied immediately before a write or render — never stored as if safe.
///
/// Stripping runs first, but NOT to stop a separator smuggling itself past the
/// path check — a `/` is a `/` whatever direction marks surround it, so that
/// ordering buys nothing there. (Established by breaking it: moving the path
/// check onto the raw name fails no test, because it cannot.)
///
/// It runs first for two real reasons:
/// - the value we RETURN is written to disk and shown in the UI, so it must
///   carry no overrides or control characters — that is the deceptive-display
///   attack, and it is a property of the OUTPUT, not of the checks;
/// - the extension must be judged on the real characters, so an honest
///   `photo.p\u{202E}ng` resolves to `png` instead of being refused.
pub fn safe_filename(raw: &str) -> Result<String, FileRefused> {
    // 1. Remove control characters, bidirectional overrides, and NULs. These
    //    make `invoice.exe` present itself as `invoice.png` in a UI, which is
    //    the whole trick.
    let cleaned: String = raw
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    // Bidi controls: LRE RLE PDF LRO RLO, LRI RLI FSI PDI, LRM RLM ALM
                    '\u{202A}'..='\u{202E}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{200E}'
                        | '\u{200F}'
                        | '\u{061C}'
                )
        })
        .collect();

    // 2. Any path structure at all is a refusal, not something to trim. A name
    //    that contains a separator is trying to choose where it lands.
    if cleaned.contains('/') || cleaned.contains('\\') {
        return Err(FileRefused::PathInName);
    }
    // Windows drive/stream separators, and traversal in any position.
    if cleaned.contains(':') || cleaned == ".." || cleaned.starts_with("../") {
        return Err(FileRefused::PathInName);
    }

    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        return Err(FileRefused::EmptyName);
    }
    if trimmed.len() > MAX_FILENAME_LEN {
        return Err(FileRefused::NameTooLong);
    }

    // 3. The extension allowlist, applied to the FINAL extension — which is the
    //    one the operating system will act on. `invoice.pdf.exe` is an `exe`.
    let ext = trimmed
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext.is_empty() {
        return Err(FileRefused::ExtensionNotAllowed(String::new()));
    }
    if !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
        return Err(FileRefused::ExtensionNotAllowed(ext));
    }

    Ok(trimmed.to_string())
}

/// Check a file's real bytes against the type it claims to be.
///
/// Unrecognized bytes are a refusal: for a sealed attachment nothing else has
/// inspected these bytes, so "I do not know what this is" cannot mean "write it
/// to disk anyway".
///
/// # Text is the one format with no magic bytes
///
/// `ALLOWED_EXTENSIONS` carries `txt`, `md`, `csv`, `json` and `log`, and none of
/// them can ever be sniffed — plain text has no signature. Left alone, the two
/// halves of the policy contradict each other: the name check allows the file and
/// the content check always refuses it, so a `.txt` could never be sent at all.
///
/// A textual CLAIM is therefore checked against textual CONTENT instead of against
/// a signature: valid UTF-8, no NULs, no control characters beyond tab/CR/LF. That
/// still refuses the attack this check exists for — an ELF or a PE renamed to
/// `.txt` is not valid text, and a PNG renamed to `.txt` sniffs as `image/png` and
/// contradicts its claim before this branch is reached.
pub fn check_contents(bytes: &[u8], claimed_mime: &str) -> Result<SniffedType, FileRefused> {
    let sniffed = sniff(bytes);
    match sniffed {
        Some(t) if mime_agrees(t.mime, claimed_mime) => Ok(t),
        None if is_textual_mime(claimed_mime) && looks_like_text(bytes) => {
            Ok(SniffedType { mime: "text/plain" })
        }
        other => Err(FileRefused::ContentMismatch {
            claimed: claimed_mime.to_string(),
            sniffed: other.map(|t| t.mime.to_string()),
        }),
    }
}

/// The claimed types that have no magic bytes and are judged as text instead.
fn is_textual_mime(claimed: &str) -> bool {
    let c = claimed.to_ascii_lowercase();
    c.starts_with("text/") || c == "application/json"
}

/// Whether these bytes are plausibly text: valid UTF-8, no NUL, and no control
/// characters other than tab, CR and LF.
///
/// Deliberately strict rather than heuristic. Binaries renamed to `.txt` are the
/// thing being refused, and every executable format in practice carries NULs or
/// control bytes in its first few hundred bytes.
fn looks_like_text(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    !text
        .chars()
        .any(|c| c.is_control() && c != '\t' && c != '\r' && c != '\n')
}

/// Whether a sniffed MIME and a claimed one describe the same thing.
///
/// Exact match, plus the container aliases that are genuinely one format wearing
/// two names: a WebM/Matroska container carries audio or video, and an MP4
/// container likewise. Everything else must match exactly — a loose comparison
/// here would quietly undo the whole check.
fn mime_agrees(sniffed: &str, claimed: &str) -> bool {
    if sniffed.eq_ignore_ascii_case(claimed) {
        return true;
    }
    matches!(
        (sniffed, claimed),
        ("video/webm", "audio/webm")
            | ("video/mp4", "audio/mp4")
            // QuickTime and MP4 share the ISO base media container, so the same
            // `ftyp` signature covers a .mov as well.
            | ("video/mp4", "video/quicktime")
            | ("audio/mpeg", "audio/mp3")
    )
}

// ---------------------------------------------------------------------------
// Sealing a file (F3)
// ---------------------------------------------------------------------------

/// Seal one file's bytes under a FRESH random key.
///
/// The key travels inside the message ciphertext, never beside the blob. Because
/// every file gets its own random key, two uploads of the same file produce
/// unrelated ciphertexts — so the blob's hash is uncorrelatable across uploads
/// and servers, which is strictly better than Rung 1's plaintext-hash
/// existence oracle.
///
/// Reuses the crate's existing AES-256-GCM primitive rather than introducing a
/// second construction: file bytes are not special, and a second AEAD path would
/// be a second thing to get wrong.
pub fn seal_file(plaintext: &[u8]) -> anyhow::Result<([u8; 32], Vec<u8>)> {
    let key: [u8; 32] = rand::random();
    let ciphertext = crate::encryption::encrypt(&key, plaintext)?;
    Ok((key, ciphertext))
}

/// Open a sealed file. Fails closed on a wrong key or any tampering — AES-GCM
/// authenticates, so a flipped byte is a refusal rather than garbage bytes
/// handed to a renderer.
pub fn open_file(key: &[u8; 32], ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
    crate::encryption::decrypt(key, ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Hostile filenames (F2, the named deliverable) ----------------------
    //
    // Every case asserts the SPECIFIC refusal. "Returns an error" would pass
    // even if the wrong rule fired, which is how a sanitizer rots into a
    // decoration that happens to reject things.

    #[test]
    fn unix_traversal_is_refused() {
        assert_eq!(
            safe_filename("../../.ssh/authorized_keys"),
            Err(FileRefused::PathInName)
        );
    }

    #[test]
    fn windows_paths_and_drive_letters_are_refused() {
        assert_eq!(safe_filename(r"C:\Windows\System32\evil.png"), Err(FileRefused::PathInName));
        assert_eq!(safe_filename(r"..\..\evil.png"), Err(FileRefused::PathInName));
        // An NTFS alternate data stream hides content behind a legitimate name.
        assert_eq!(safe_filename("holiday.png:hidden.exe"), Err(FileRefused::PathInName));
    }

    #[test]
    fn a_double_extension_is_judged_by_its_LAST_extension() {
        // `invoice.pdf.exe` is an exe. The operating system agrees; so must we.
        assert_eq!(
            safe_filename("invoice.pdf.exe"),
            Err(FileRefused::ExtensionNotAllowed("exe".to_string()))
        );
        // And the honest version of the same name is fine.
        assert_eq!(safe_filename("invoice.pdf").unwrap(), "invoice.pdf");
    }

    #[test]
    fn a_right_to_left_override_cannot_disguise_an_extension() {
        // The classic: U+202E makes "exe.png" render as "gnp.exe" in most UIs.
        // Stripping the override reveals what the name really ends with.
        let disguised = "photo\u{202E}gnp.exe";
        assert_eq!(
            safe_filename(disguised),
            Err(FileRefused::ExtensionNotAllowed("exe".to_string())),
            "the override must be stripped BEFORE the extension is judged"
        );
    }

    #[test]
    fn bidi_marks_are_stripped_from_an_otherwise_fine_name() {
        let sneaky = "holi\u{200F}day\u{2066}.png";
        assert_eq!(safe_filename(sneaky).unwrap(), "holiday.png");
    }

    #[test]
    fn control_characters_and_nul_are_stripped() {
        assert_eq!(safe_filename("re\u{0}port\n.pdf").unwrap(), "report.pdf");
    }

    #[test]
    fn a_separator_next_to_an_override_is_still_a_separator() {
        assert_eq!(safe_filename("evil\u{202E}/x.png"), Err(FileRefused::PathInName));
    }

    /// The property the stripping actually buys, stated as a test because the
    /// COMMENT version of it was wrong until a break-test caught it: whatever we
    /// return gets written to disk and shown in the UI, so it must never carry a
    /// direction override or control character, wherever in the name they sat.
    #[test]
    fn the_returned_name_never_contains_deceptive_characters() {
        for raw in [
            "holi\u{200F}day.png",
            "a\u{202A}b\u{202B}c\u{202C}.pdf",
            "re\u{0}port\u{7}.txt",
            "\u{2066}wrapped\u{2069}.md",
        ] {
            let safe = safe_filename(raw).expect("these are otherwise fine names");
            assert!(
                !safe.chars().any(|c| c.is_control()
                    || matches!(c,
                        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
                            | '\u{200E}' | '\u{200F}' | '\u{061C}')),
                "{raw:?} produced {safe:?}, which still carries a deceptive character"
            );
        }
    }

    /// An HONEST file whose extension merely contains a stray mark still resolves
    /// to its real extension — the other half of why stripping precedes the
    /// extension check.
    #[test]
    fn a_stray_mark_inside_an_extension_does_not_refuse_an_honest_file() {
        assert_eq!(safe_filename("photo.p\u{202E}ng").unwrap(), "photo.png");
    }

    #[test]
    fn empty_and_dots_only_names_are_refused() {
        assert_eq!(safe_filename(""), Err(FileRefused::EmptyName));
        assert_eq!(safe_filename("   "), Err(FileRefused::EmptyName));
        assert_eq!(safe_filename("..."), Err(FileRefused::EmptyName));
        assert_eq!(safe_filename("\u{202E}\u{200F}"), Err(FileRefused::EmptyName));
    }

    #[test]
    fn an_absurdly_long_name_is_refused() {
        let long = format!("{}.png", "a".repeat(MAX_FILENAME_LEN));
        assert_eq!(safe_filename(&long), Err(FileRefused::NameTooLong));
    }

    #[test]
    fn a_name_with_no_extension_is_refused() {
        assert_eq!(
            safe_filename("README"),
            Err(FileRefused::ExtensionNotAllowed(String::new()))
        );
    }

    #[test]
    fn ordinary_names_survive_unchanged() {
        for name in ["holiday.png", "voice-message.ogg", "notes.md", "Report 2026.pdf"] {
            assert_eq!(safe_filename(name).unwrap(), name, "{name} should pass through");
        }
    }

    // -- Magic-byte sniffing ------------------------------------------------

    #[test]
    fn sniff_identifies_the_formats_the_server_would_have() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....").unwrap().mime, "image/png");
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap().mime, "image/jpeg");
        assert_eq!(sniff(b"GIF89a...").unwrap().mime, "image/gif");
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBP").unwrap().mime, "image/webp");
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVE").unwrap().mime, "audio/wav");
        assert_eq!(sniff(b"OggS....").unwrap().mime, "audio/ogg");
        assert_eq!(sniff(b"%PDF-1.7").unwrap().mime, "application/pdf");
    }

    #[test]
    fn unrecognized_bytes_sniff_to_nothing() {
        assert!(sniff(b"just some text").is_none());
        assert!(sniff(&[]).is_none());
        assert!(sniff(&[0x7F, b'E', b'L', b'F']).is_none(), "an ELF is not on the allowlist");
    }

    /// The check that makes the extension allowlist mean something: a file can
    /// claim to be a PNG and be an executable.
    #[test]
    fn bytes_that_contradict_the_claim_are_refused() {
        let elf = [0x7F, b'E', b'L', b'F', 1, 1, 1, 0];
        assert_eq!(
            check_contents(&elf, "image/png"),
            Err(FileRefused::ContentMismatch {
                claimed: "image/png".to_string(),
                sniffed: None,
            })
        );

        // A real PNG claiming to be plain text is equally a mismatch — the
        // check is symmetric, not "is it dangerous".
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0";
        assert!(matches!(
            check_contents(png, "text/plain"),
            Err(FileRefused::ContentMismatch { .. })
        ));
    }

    #[test]
    fn honest_files_pass_the_content_check() {
        assert_eq!(
            check_contents(b"\x89PNG\r\n\x1a\n\0", "image/png").unwrap().mime,
            "image/png"
        );
        // Container aliases are the ONE allowed looseness.
        assert!(check_contents(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0], "audio/webm").is_ok());
    }

    #[test]
    fn a_refusal_explains_itself_in_words_a_person_can_read() {
        let msg = FileRefused::ExtensionNotAllowed("exe".to_string()).to_string();
        assert!(msg.contains("not allowed"), "got {msg}");
        let msg = FileRefused::PathInName.to_string();
        assert!(msg.contains("folder path"), "got {msg}");
    }

    // --- Text: the format with no signature (see `check_contents`) ---

    #[test]
    fn a_real_text_file_is_accepted_for_a_textual_claim() {
        // Without this branch the allowlist and the sniffer contradict each
        // other and a .txt can never be sent at all.
        let text = b"hello\tworld\r\n- a list\n";
        assert_eq!(check_contents(text, "text/plain").unwrap().mime, "text/plain");
        assert_eq!(check_contents(b"{\"a\": 1}", "application/json").unwrap().mime, "text/plain");
        assert_eq!(check_contents(b"a,b,c\n1,2,3\n", "text/csv").unwrap().mime, "text/plain");
        // Empty is vacuously text, and an empty note is not an attack.
        assert!(check_contents(b"", "text/plain").is_ok());
    }

    #[test]
    fn a_binary_renamed_to_text_is_still_refused() {
        // The attack the check exists for: an executable wearing a .txt name.
        let elf = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00";
        assert!(matches!(
            check_contents(elf, "text/plain"),
            Err(FileRefused::ContentMismatch { sniffed: None, .. }),
        ));
        // A PE/DOS stub, likewise -- NULs are not text.
        let pe = b"MZ\x90\x00\x03\x00\x00\x00";
        assert!(check_contents(pe, "text/plain").is_err());
        // Invalid UTF-8 is not text either.
        assert!(check_contents(&[0xff, 0xfe, 0x00, 0x41], "text/plain").is_err());
    }

    #[test]
    fn a_sniffable_format_still_wins_over_a_textual_claim() {
        // The textual branch is only reached when NOTHING sniffed; a PNG
        // claiming to be text is caught by the claim check, as before.
        let png = b"\x89PNG\r\n\x1a\n and then some";
        assert!(matches!(
            check_contents(png, "text/plain"),
            Err(FileRefused::ContentMismatch { sniffed: Some(s), .. }) if s == "image/png",
        ));
    }

    #[test]
    fn quicktime_and_mp4_share_the_iso_container() {
        // .mov is on the allowlist; the same `ftyp` signature covers it.
        let mov = b"\x00\x00\x00\x18ftypqt  \x00\x00\x00\x00";
        assert_eq!(check_contents(mov, "video/quicktime").unwrap().mime, "video/mp4");
    }
}

#[cfg(test)]
mod seal_tests {
    use super::*;

    #[test]
    fn a_sealed_file_round_trips() {
        let bytes = b"\x89PNG\r\n\x1a\n and then some picture data".to_vec();
        let (key, ciphertext) = seal_file(&bytes).unwrap();
        assert_ne!(ciphertext, bytes, "the blob must not be the plaintext");
        assert!(
            !ciphertext.windows(4).any(|w| w == b"\x89PNG"),
            "the plaintext's magic bytes must not survive into the blob"
        );
        assert_eq!(open_file(&key, &ciphertext).unwrap(), bytes);
    }

    #[test]
    fn the_wrong_key_cannot_open_it() {
        let (_key, ciphertext) = seal_file(b"secret document").unwrap();
        assert!(open_file(&[7u8; 32], &ciphertext).is_err());
    }

    #[test]
    fn a_tampered_blob_is_refused_rather_than_returned_as_garbage() {
        let (key, mut ciphertext) = seal_file(b"secret document").unwrap();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x01;
        assert!(
            open_file(&key, &ciphertext).is_err(),
            "AES-GCM authenticates: tampering must be a refusal, never bytes handed to a renderer"
        );
    }

    #[test]
    fn a_truncated_blob_is_refused() {
        let (key, ciphertext) = seal_file(b"secret document").unwrap();
        assert!(open_file(&key, &ciphertext[..ciphertext.len() / 2]).is_err());
    }

    /// Each file gets its own key, so the same bytes uploaded twice produce
    /// unrelated blobs — the property that removes Rung 1's existence oracle.
    #[test]
    fn the_same_file_sealed_twice_gives_uncorrelatable_blobs() {
        let bytes = b"the very same file";
        let (k1, c1) = seal_file(bytes).unwrap();
        let (k2, c2) = seal_file(bytes).unwrap();
        assert_ne!(k1, k2, "each file gets a fresh key");
        assert_ne!(c1, c2, "identical input must not produce an identical blob");
        assert!(open_file(&k2, &c1).is_err(), "keys are not interchangeable");
    }

    /// The whole point of the pairing: what the server stores is opaque, and
    /// what the client renders is checked BEFORE it is trusted.
    #[test]
    fn a_sealed_blob_reveals_nothing_the_policy_would_have_judged() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        let (key, ciphertext) = seal_file(&png).unwrap();
        // The server, holding only the blob, cannot sniff it.
        assert!(sniff(&ciphertext).is_none(), "a sealed blob must not sniff as a real format");
        // The client, after opening it, can — and must, before rendering.
        let opened = open_file(&key, &ciphertext).unwrap();
        assert_eq!(check_contents(&opened, "image/png").unwrap().mime, "image/png");
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;

    /// The property the sealed-download path depends on: a sanitized name can
    /// never escape the directory it is joined to.
    ///
    /// This is what makes `downloads.join(&file_name)` safe, and it is asserted
    /// here rather than trusted, because "the name was sanitized earlier" is
    /// exactly the assumption that rots when someone reorders the steps.
    #[test]
    fn a_sanitized_name_cannot_escape_its_directory() {
        let base = std::path::Path::new("/home/user/Downloads");
        for hostile in [
            "../../.ssh/authorized_keys",
            r"..\..\evil.png",
            "/etc/passwd",
            "sub/dir/file.png",
        ] {
            // Every one of these is refused outright...
            assert!(
                safe_filename(hostile).is_err(),
                "{hostile:?} should never survive sanitization"
            );
        }
        // ...and anything that DOES survive stays inside the directory.
        for ok in ["holiday.png", "notes.md", "voice.ogg", "Report 2026.pdf"] {
            let safe = safe_filename(ok).unwrap();
            let joined = base.join(&safe);
            assert_eq!(
                joined.parent(),
                Some(base),
                "{safe:?} escaped its directory when joined"
            );
        }
    }

    /// The recipient must interpret bytes by what they ARE, not by what the
    /// sender said. A sender who labels an ELF as a PNG gets a refusal, so no
    /// path exists where a renderer is handed bytes of an unexpected type.
    #[test]
    fn the_sender_does_not_get_to_choose_how_their_bytes_are_read() {
        let elf = [0x7F, b'E', b'L', b'F', 1, 1, 1, 0];
        assert!(check_contents(&elf, "image/png").is_err());

        // And for an honest file, the type used downstream is the SNIFFED one.
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0";
        assert_eq!(check_contents(png, "image/png").unwrap().mime, "image/png");
    }
}
