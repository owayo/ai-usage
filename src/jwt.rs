//! JWT の payload を読む。署名は検証しない。
//!
//! provider の access token から有効期限や account 情報を読むためだけに使い、
//! token の真正性の判断には使わない。

use base64::Engine;
use serde_json::Value;

/// JWT の payload(2 番目の segment)を JSON として返す。
/// base64url の padding は有無どちらも受け付ける。形式が壊れていれば `None`。
pub fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let mut b64 = payload.replace('-', "+").replace('_', "/");
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
    use serde_json::json;

    #[test]
    fn claims_reads_unpadded_and_padded_base64url_payloads() {
        // `?` と `>` を含めて、標準 Base64 では `/` `+` になる文字を base64url 側で出す。
        let payload = json!({"exp": 1_784_117_357_i64, "email": "a@example.com", "s": "??>"});
        let bytes = serde_json::to_vec(&payload).unwrap();
        let padded = URL_SAFE.encode(&bytes);
        assert!(padded.ends_with('='), "padding 付きの形を検証できていない");
        assert!(
            padded.contains(['-', '_']),
            "base64url 固有の文字を検証できていない"
        );
        for body in [URL_SAFE_NO_PAD.encode(&bytes), padded] {
            assert_eq!(claims(&format!("hdr.{body}.sig")), Some(payload.clone()));
        }
    }

    #[test]
    fn claims_rejects_tokens_without_a_json_payload() {
        let not_json = format!("hdr.{}.sig", URL_SAFE_NO_PAD.encode("not json"));
        for token in ["", "only-one-segment", "hdr.@@@.sig", not_json.as_str()] {
            assert_eq!(claims(token), None, "{token:?}");
        }
    }
}
