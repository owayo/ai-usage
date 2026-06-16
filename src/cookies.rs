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

/// AES-128-CBC で平文を暗号化して "v10"|"v11" 形式の blob を組み立てるテスト用ヘルパ。
/// `decrypt` の round-trip を検証するためだけに `#[cfg(test)]` で提供する。
#[cfg(test)]
fn encrypt_for_test(key: &[u8; 16], prefix: &[u8], plain: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let iv = [0x20u8; 16];
    let ct = Aes128CbcEnc::new(&(*key).into(), &iv.into()).encrypt_padded_vec::<Pkcs7>(plain);
    let mut out = Vec::with_capacity(prefix.len() + ct.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(&ct);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 16] {
        // derive_key の結果は再現可能だが、テストでは固定のダミー鍵を使うだけで十分。
        [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00,
        ]
    }

    #[test]
    fn decrypt_round_trip_v10() {
        // v10 prefix + AES-128-CBC で暗号化したものを復号できる。
        let blob = encrypt_for_test(&key(), b"v10", b"hello");
        assert_eq!(decrypt(&key(), &blob, false).as_deref(), Some("hello"));
    }

    #[test]
    fn decrypt_strips_sha256_prefix_for_schema_v24() {
        // schema v24+ は平文の先頭 32 バイト(SHA-256 ハッシュ)を取り除く必要がある。
        let mut plain = vec![0u8; 32];
        plain.extend_from_slice(b"value");
        let blob = encrypt_for_test(&key(), b"v10", &plain);
        assert_eq!(decrypt(&key(), &blob, true).as_deref(), Some("value"));
    }

    #[test]
    fn decrypt_returns_none_for_unknown_prefix() {
        // v20 など macOS で扱わない方式は明示的に弾く(プロセス全体の失敗にしない)。
        let blob = encrypt_for_test(&key(), b"v20", b"x");
        assert!(decrypt(&key(), &blob, false).is_none());
    }

    #[test]
    fn decrypt_returns_none_for_too_short_blob() {
        // 3 バイトの prefix + 16 バイトの IV ブロック未満は無条件に弾く。
        assert!(decrypt(&key(), b"v10short", false).is_none());
    }

    #[test]
    fn decrypt_returns_none_with_wrong_key() {
        let blob = encrypt_for_test(&key(), b"v10", b"hello");
        let mut other = key();
        other[0] ^= 0xff;
        // 復号自体が padding 不一致で失敗するか、UTF-8 にならず None が返る。
        assert!(decrypt(&other, &blob, false).is_none());
    }

    #[test]
    fn derive_key_is_deterministic() {
        // 同じパスワードからは常に同じ鍵が得られる(PBKDF2 のソルトと反復回数は固定)。
        let a = derive_key("password");
        let b = derive_key("password");
        assert_eq!(a, b);
        assert_ne!(derive_key("password"), derive_key("PASSWORD"));
    }
}
