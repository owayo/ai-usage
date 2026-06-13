//! Decryption of Chrome cookies on macOS (the classic `v10` scheme).
//!
//! macOS Chrome (incl. 149) encrypts `encrypted_value` as
//! `"v10" + AES-128-CBC(IV = 16 × 0x20)`, with the AES key derived via
//! `PBKDF2-HMAC-SHA1(keychain "Chrome Safe Storage" secret, "saltysalt", 1003)`.
//! Cookie-store schema v24+ additionally prepends a 32-byte SHA-256 of the host
//! key to the plaintext, which must be stripped. (Windows' `v20` app-bound
//! encryption does not apply on macOS.)

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, anyhow};
use sha1::Sha1;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Read the "Chrome Safe Storage" secret from the macOS login Keychain.
///
/// The first time a given binary requests this, macOS shows an approval dialog;
/// choosing "Always Allow" prevents future prompts.
pub fn safe_storage_key(service: &str) -> Result<String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .context("failed to run `security`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "could not read the '{service}' key from Keychain — approve the macOS prompt (\"Always Allow\") and retry. {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn derive_key(password: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), b"saltysalt", 1003, &mut key);
    key
}

/// Decrypt a single `encrypted_value` blob. Returns `None` for blobs we can't
/// handle (empty, unknown prefix, or a different key).
fn decrypt(key: &[u8; 16], blob: &[u8], strip_sha256_prefix: bool) -> Option<String> {
    if blob.len() < 3 + 16 {
        return None;
    }
    match &blob[0..3] {
        b"v10" | b"v11" => {}
        _ => return None, // v20 (Windows app-bound) is not used on macOS
    }
    let iv = [0x20u8; 16];
    let plain = Aes128CbcDec::new(&(*key).into(), &iv.into())
        .decrypt_padded_vec::<Pkcs7>(&blob[3..])
        .ok()?;
    let value = if strip_sha256_prefix && plain.len() >= 32 {
        &plain[32..]
    } else {
        &plain[..]
    };
    String::from_utf8(value.to_vec()).ok()
}

#[derive(Default)]
pub struct ProfileCookies {
    /// Decrypted claude.ai cookies, keyed by name.
    pub claude: HashMap<String, String>,
    /// Decrypted chatgpt.com cookies, keyed by name.
    pub chatgpt: HashMap<String, String>,
}

/// Open the (live, locked) Cookies DB read-only and decrypt the claude.ai and
/// chatgpt.com cookies.
pub fn load(db_path: &Path, key: &[u8; 16]) -> Result<ProfileCookies> {
    use rusqlite::{Connection, OpenFlags};

    let uri = format!("file:{}?immutable=1", db_path.to_string_lossy());
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening cookie DB {}", db_path.display()))?;

    let schema: i64 = conn
        .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let strip = schema >= 24;

    let mut stmt = conn.prepare("SELECT host_key, name, encrypted_value FROM cookies")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;

    let mut out = ProfileCookies::default();
    for row in rows {
        let (host, name, enc) = row?;
        if enc.is_empty() {
            continue;
        }
        let host = host.trim_start_matches('.');
        let bucket = if host.ends_with("claude.ai") {
            &mut out.claude
        } else if host.ends_with("chatgpt.com") {
            &mut out.chatgpt
        } else {
            continue;
        };
        if let Some(value) = decrypt(key, &enc, strip) {
            bucket.insert(name, value);
        }
    }
    Ok(out)
}

/// Cheaply detect which providers a profile is signed into, by cookie *presence*
/// only — no decryption, no Keychain. Returns `(has_claude, has_codex)`.
/// Used by `--init-config`.
pub fn detect_sessions(db_path: &Path) -> (bool, bool) {
    use rusqlite::{Connection, OpenFlags};
    let uri = format!("file:{}?immutable=1", db_path.to_string_lossy());
    let Ok(conn) = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return (false, false);
    };
    let exists = |sql: &str| conn.query_row(sql, [], |_| Ok(())).is_ok();
    let claude = exists(
        "SELECT 1 FROM cookies WHERE host_key LIKE '%claude.ai' AND name = 'sessionKey' LIMIT 1",
    );
    let codex = exists(
        "SELECT 1 FROM cookies WHERE host_key LIKE '%chatgpt.com' AND name LIKE '__Secure-next-auth.session-token%' LIMIT 1",
    );
    (claude, codex)
}
