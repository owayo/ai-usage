//! macOS Chrome Cookie の復号(macOS で使われる従来の `v10` 方式)。
//!
//! macOS Chrome 149 までの `encrypted_value` は `"v10" +
//! AES-128-CBC(IV = 16 × 0x20)` で、AES 鍵は Keychain の
//! `"Chrome Safe Storage"` シークレットから
//! `PBKDF2-HMAC-SHA1(..., "saltysalt", 1003)` で導出する。Cookie DB の
//! schema v24+ は平文先頭に host key の SHA-256 32 バイトを付加するため、復号後に
//! 取り除く必要がある。Windows の `v20` app-bound encryption は macOS では扱わない。

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, anyhow};
use sha1::Sha1;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

const CLAUDE_DOMAIN: &str = "claude.ai";
const CHATGPT_DOMAIN: &str = "chatgpt.com";
const CODEX_SESSION_COOKIE: &str = "__Secure-next-auth.session-token";

/// macOS login Keychain から "Chrome Safe Storage" シークレットを読む。
/// バイナリごとの初回アクセス時は macOS の許可ダイアログが出るため、
/// "Always Allow" を選ぶと以後のプロンプトを避けられる。
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

/// 単一の `encrypted_value` blob を復号する。空、未知 prefix、鍵違いなど
/// この実装で扱えない blob は `None` を返す。
fn decrypt(key: &[u8; 16], blob: &[u8], strip_sha256_prefix: bool) -> Option<String> {
    if blob.len() < 3 + 16 {
        return None;
    }
    match &blob[0..3] {
        b"v10" | b"v11" => {}
        _ => return None, // v20(Windows app-bound) は macOS 対象外。
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
    /// 復号済み claude.ai Cookie。キーは Cookie 名。
    pub claude: HashMap<String, String>,
    /// 復号済み chatgpt.com Cookie。キーは Cookie 名。
    pub chatgpt: HashMap<String, String>,
}

fn sqlite_immutable_uri(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let path = path.to_string_lossy();
    let mut uri = String::with_capacity(path.len() + 24);
    uri.push_str("file:");
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(b as char);
            }
            _ => {
                uri.push('%');
                uri.push(HEX[(b >> 4) as usize] as char);
                uri.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    uri.push_str("?immutable=1");
    uri
}

fn host_matches_domain(host: &str, domain: &str) -> bool {
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    host == domain
}

fn is_codex_session_cookie(name: &str) -> bool {
    name == CODEX_SESSION_COOKIE
        || name
            .strip_prefix(CODEX_SESSION_COOKIE)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

/// live でロックされがちな Cookies DB を read-only + immutable で開き、
/// claude.ai / chatgpt.com の Cookie だけを復号する。
pub fn load(db_path: &Path, key: &[u8; 16]) -> Result<ProfileCookies> {
    use rusqlite::{Connection, OpenFlags};

    let uri = sqlite_immutable_uri(db_path);
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
        let bucket = if host_matches_domain(&host, CLAUDE_DOMAIN) {
            &mut out.claude
        } else if host_matches_domain(&host, CHATGPT_DOMAIN) {
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

/// Cookie の存在だけでログイン済みプロバイダを安価に検出する。復号も Keychain 参照も
/// 行わない。戻り値は `(has_claude, has_codex)`。`--init-config` で使う。
pub fn detect_sessions(db_path: &Path) -> (bool, bool) {
    use rusqlite::{Connection, OpenFlags};
    let uri = sqlite_immutable_uri(db_path);
    let Ok(conn) = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return (false, false);
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT host_key, name FROM cookies \
         WHERE name = 'sessionKey' OR name GLOB '__Secure-next-auth.session-token*'",
    ) else {
        return (false, false);
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return (false, false);
    };
    let mut claude = false;
    let mut codex = false;
    for (host, name) in rows.flatten() {
        if name == "sessionKey" && host_matches_domain(&host, CLAUDE_DOMAIN) {
            claude = true;
        }
        if is_codex_session_cookie(&name) && host_matches_domain(&host, CHATGPT_DOMAIN) {
            codex = true;
        }
        if claude && codex {
            break;
        }
    }
    (claude, codex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn key() -> [u8; 16] {
        // derive_key の結果は再現可能だが、テストでは固定のダミー鍵を使うだけで十分。
        [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00,
        ]
    }

    fn temp_cookie_db(name: &str, rows: &[(&str, &str)]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ai-usage-cookies-{name}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE cookies (
                host_key TEXT NOT NULL,
                name TEXT NOT NULL,
                encrypted_value BLOB NOT NULL DEFAULT x''
            )",
            [],
        )
        .unwrap();
        for (host, cookie_name) in rows {
            conn.execute(
                "INSERT INTO cookies (host_key, name) VALUES (?1, ?2)",
                rusqlite::params![host, cookie_name],
            )
            .unwrap();
        }
        drop(conn);
        path
    }

    #[test]
    fn sqlite_immutable_uri_escapes_query_delimiters() {
        let uri = sqlite_immutable_uri(Path::new("/tmp/ai usage?#.sqlite"));
        assert_eq!(uri, "file:/tmp/ai%20usage%3F%23.sqlite?immutable=1");
    }

    #[test]
    fn host_matching_requires_domain_boundary() {
        assert!(host_matches_domain("claude.ai", CLAUDE_DOMAIN));
        assert!(host_matches_domain(".claude.ai", CLAUDE_DOMAIN));
        assert!(!host_matches_domain("console.claude.ai", CLAUDE_DOMAIN));
        assert!(!host_matches_domain("evilclaude.ai", CLAUDE_DOMAIN));
        assert!(!host_matches_domain("claude.ai.evil.test", CLAUDE_DOMAIN));
    }

    #[test]
    fn detect_sessions_accepts_real_provider_domains() {
        let path = temp_cookie_db(
            "real-domains",
            &[
                (".claude.ai", "sessionKey"),
                (".chatgpt.com", &format!("{CODEX_SESSION_COOKIE}.0")),
            ],
        );
        assert_eq!(detect_sessions(&path), (true, true));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn detect_sessions_rejects_suffix_lookalike_domains_and_cookie_names() {
        let path = temp_cookie_db(
            "lookalikes",
            &[
                ("evilclaude.ai", "sessionKey"),
                ("evilchatgpt.com", CODEX_SESSION_COOKIE),
                ("chatgpt.com", "xxSecure-next-auth.session-token"),
                ("chatgpt.com", "__Secure-next-auth.session-tokenizer"),
            ],
        );
        assert_eq!(detect_sessions(&path), (false, false));
        let _ = std::fs::remove_file(path);
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
