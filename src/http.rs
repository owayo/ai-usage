//! 共有 HTTP client。
//!
//! `browser` は Chrome TLS/HTTP2 emulation 付きの `wreq` を使い、replay した
//! `cf_clearance` Cookie を Cloudflare に受け入れさせる。plain client は fingerprint で
//! 403 "Just a moment" challenge になるため、claude.ai と chatgpt.com では使えない。
//! `api` は Google `cloudcode-pa` と、通常の Bearer 認証を受け付ける Grok 用のクライアント。

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use wreq::{Client, Response, StatusCode};
use wreq_util::Emulation;

/// 1 リクエストの全体 deadline。通常の fetch は 1〜2 秒で完了するため、ここに
/// かかるのは相手側が応答を返さずぶら下がっている場合だけ。timeout は send() の
/// transport error として返り、retryable 扱いで backoff 再試行に回る。
/// 未設定だと遅い接続を無期限に待ち、1 本のハングが全体の応答時間を分単位まで
/// 引き延ばす(実測で 48 秒の実行を観測)。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// installed Chrome と合わせた User-Agent。その Chrome で発行された `cf_clearance` Cookie を
/// Cloudflare に有効と判定させる。`Emulation::Chrome149` の既定 UA と同一だが、TLS/HTTP2
/// fingerprint と UA の版数対応を見える形で固定するため、定数として明示している。
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

/// Cloudflare-fronted site 用の browser-emulating client と Google API 用 plain client。
#[derive(Clone)]
pub struct Clients {
    pub browser: Client,
    pub api: Client,
}

/// 同じ request を backoff 後に再試行する価値がある transport / HTTP failure。
/// provider の parse/auth error と区別するため、anyhow の source chain に marker を残す。
#[derive(Debug)]
struct RetryableHttpError(String);

impl fmt::Display for RetryableHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RetryableHttpError {}

/// 共通リトライ処理へ伝える一時的な通信エラーを生成する。
/// provider 固有の HTTP リクエストでも同じ marker を付けられるよう crate 内に公開する。
pub(crate) fn retryable_error(message: String) -> anyhow::Error {
    anyhow::Error::new(RetryableHttpError(message))
}

pub fn is_retryable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<RetryableHttpError>().is_some())
}

/// refresh token を送信した後のエラーから retryable marker を落とす。
///
/// PixelLab(Supabase)と Grok は refresh のたびに refresh token を rotation するが、
/// ai-usage は Cookie / `auth.json` を書き戻さない読み取り専用ツールなので、保存された
/// token の世代は進まない。ここで marker を残すと `main.rs` の `fetch_with_retry` が
/// provider の `fetch` 全体をやり直し、ディスク上の同じ(古い)refresh token をもう一度
/// 送ってしまう。refresh の応答を受け取れなかった場合はサーバ側で rotation 済みか
/// 判別できず、reuse detection を持つ発行側では session ごと失効し得る
/// (1 リクエストの deadline は 10 秒なので、backoff を挟んだ再送は Supabase の
/// 既定 reuse 猶予をまたぎ得る)。一時的な失敗で 1 行が空くより、資格情報を焼かない
/// 方を選ぶ。
pub(crate) fn no_retry_after_refresh(error: anyhow::Error) -> anyhow::Error {
    if is_retryable(&error) {
        // marker は source chain に埋まっているため、chain ごと 1 つの message に畳む。
        // `{:#}` は chain 全体を ": " 区切りで連結するので、原因の文面は失われない。
        anyhow!("{error:#}")
    } else {
        error
    }
}

/// 待機後の再試行で回復し得る HTTP status を判定する。
/// GET / POST の経路差で再試行ポリシーがずれないよう、ここを単一の正本にする。
pub(crate) fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

pub fn clients() -> Result<Clients> {
    let browser = Client::builder()
        // UA 定数と同じ Chrome 149 の TLS/HTTP2/ヘッダ(`sec-ch-ua` のブランド版数を含む)
        // プリセット。emulation は既存の HTTP1/HTTP2/TLS 設定を上書きするため先頭で呼ぶ。
        .emulation(Emulation::Chrome149)
        .user_agent(UA)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .context("building browser HTTP client")?;
    let api = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .context("building API HTTP client")?;
    Ok(Clients { browser, api })
}

/// `url` を GET して JSON body を parse する。Cloudflare challenge や HTTP error は
/// 分かりやすい message に変換する。browser(Chrome-emulating) client 用。
pub async fn get_json(
    client: &Client,
    url: &str,
    cookie: &str,
    bearer: Option<&str>,
    account_id: Option<&str>,
) -> Result<serde_json::Value> {
    let mut req = client.get(url);
    if !cookie.is_empty() {
        req = req.header("Cookie", cookie);
    }
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    if let Some(a) = account_id {
        req = req.header("ChatGPT-Account-Id", a);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| retryable_error(format!("GET {url}: {e}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| retryable_error(format!("reading GET response from {url}: {e}")))?;

    if !status.is_success() {
        let cloudflare =
            body.contains("Just a moment") || body.to_ascii_lowercase().contains("cloudflare");
        if cloudflare {
            // 有効な cf_clearance でも一時的に challenge が返ることがあるため、ここは意図的に
            // retryable。全試行後も続く場合にだけ、Chrome での session 更新を案内する。
            return Err(retryable_error(format!(
                "Cloudflare challenge (HTTP {}). Open the site in this Chrome profile to refresh its session, then retry.",
                status.as_u16()
            )));
        }
        let snippet: String = body.chars().take(160).collect();
        let message = format!("HTTP {} from {url}: {snippet}", status.as_u16());
        if is_retryable_status(status) {
            return Err(retryable_error(message));
        }
        return Err(anyhow!(message));
    }

    serde_json::from_str(&body).with_context(|| format!("parsing JSON from {url}"))
}

/// response body を lenient に JSON へ parse し `(status, parsed-or-Null)` を返す。
/// post_json / post_form 共通の後段処理。
async fn status_and_json(resp: Response, url: &str) -> Result<(StatusCode, serde_json::Value)> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| retryable_error(format!("reading POST response from {url}: {e}")))?;
    if is_retryable_status(status) {
        let snippet: String = text.chars().take(160).collect();
        return Err(retryable_error(format!(
            "HTTP {} from {url}: {snippet}",
            status.as_u16()
        )));
    }
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    Ok((status, json))
}

/// JSON body を POST し、`(status, parsed-or-Null)` を返す。401 → refresh+retry、
/// 403 → この token では endpoint 不許可、などの判定は caller が行う。
/// この project では `wreq` の `.json()` が必要とする `json` feature を有効化していないため、
/// body は手動 serialize する。`get_json` の手動 parse 方針にも揃えている。
pub async fn post_json(
    api: &Client,
    url: &str,
    bearer: &str,
    body: &serde_json::Value,
) -> Result<(StatusCode, serde_json::Value)> {
    let payload = serde_json::to_vec(body).context("serializing request body")?;
    let resp = api
        .post(url)
        .bearer_auth(bearer)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
        .map_err(|e| retryable_error(format!("POST {url}: {e}")))?;
    status_and_json(resp, url).await
}

/// `application/x-www-form-urlencoded` body を POST する(Google OAuth token endpoint)。
/// 戻り値は `(status, parsed-or-Null)`。
pub async fn post_form(
    api: &Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(StatusCode, serde_json::Value)> {
    let resp = api
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| retryable_error(format!("POST {url}: {e}")))?;
    status_and_json(resp, url).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::mpsc;

    /// 受けた順に決め打ちの応答を返す、テスト用の最小 HTTP/1.1 サーバー。
    /// 受け取った各リクエスト(ヘッダー部とボディ)をそのまま channel で返す。
    fn serve(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                sent.send(String::from_utf8_lossy(&request).into_owned())
                    .unwrap();
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (base, received)
    }

    /// 環境のプロキシ設定に左右されないよう、loopback のテストサーバーへ直接つなぐ。
    fn test_client() -> Client {
        Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn get_json_marks_only_transient_failures_as_retryable() {
        let (base, requests) = serve(vec![
            (200, r#"{"ok":true}"#),
            (503, "maintenance"),
            (403, "<title>Just a moment...</title>"),
            (401, r#"{"error":"invalid session"}"#),
            (200, "not json"),
        ]);
        let client = test_client();
        let url = format!("{base}/usage");

        let ok = get_json(&client, &url, "a=b", Some("token"), Some("acct"))
            .await
            .unwrap();
        assert_eq!(ok["ok"], true);
        let sent = requests.recv().unwrap().to_ascii_lowercase();
        assert!(sent.contains("cookie: a=b"), "{sent}");
        assert!(sent.contains("authorization: bearer token"), "{sent}");
        assert!(sent.contains("chatgpt-account-id: acct"), "{sent}");

        // 503 は待てば回復し得るので再試行へ回す。
        let unavailable = get_json(&client, &url, "", None, None).await.unwrap_err();
        assert!(is_retryable(&unavailable));
        assert!(
            unavailable.to_string().contains("HTTP 503"),
            "{unavailable}"
        );
        // 空の Cookie は送らない。
        assert!(
            !requests
                .recv()
                .unwrap()
                .to_ascii_lowercase()
                .contains("cookie:")
        );

        // Cloudflare の challenge は 403 でも再試行対象。provider 側の認証判定が依存する文言を固定する。
        let challenge = get_json(&client, &url, "", None, None).await.unwrap_err();
        assert!(is_retryable(&challenge));
        assert!(
            challenge
                .to_string()
                .starts_with("Cloudflare challenge (HTTP 403)."),
            "{challenge}"
        );

        // 認証失敗と壊れた JSON は待っても回復しないため、即座に返す。
        let unauthorized = get_json(&client, &url, "", None, None).await.unwrap_err();
        assert!(!is_retryable(&unauthorized));
        assert!(
            unauthorized.to_string().contains("HTTP 401")
                && unauthorized.to_string().contains("invalid session"),
            "{unauthorized}"
        );
        let malformed = get_json(&client, &url, "", None, None).await.unwrap_err();
        assert!(!is_retryable(&malformed));
        assert!(malformed.to_string().starts_with("parsing JSON from"));
    }

    #[tokio::test]
    async fn get_json_treats_connections_closed_without_a_response_as_retryable() {
        // 応答を返さずに切れた接続は、一時的な通信失敗として再試行へ回す。待受はテストの間
        // 保持し続けるため、並行して動く他のテストに同じポートを取られることもない。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                drop(stream);
            }
        });
        let error = get_json(&test_client(), &url, "", None, None)
            .await
            .unwrap_err();
        assert!(is_retryable(&error), "{error:#}");
    }

    #[tokio::test]
    async fn post_helpers_return_auth_failures_and_retry_only_transient_statuses() {
        let (base, requests) = serve(vec![
            (429, "slow down"),
            (401, r#"{"error":"invalid_grant"}"#),
            (200, "not json"),
        ]);
        let client = test_client();
        let url = format!("{base}/token");

        let limited = post_form(&client, &url, &[("grant_type", "refresh_token")])
            .await
            .unwrap_err();
        assert!(is_retryable(&limited));
        assert!(limited.to_string().contains("HTTP 429"), "{limited}");
        let sent = requests.recv().unwrap().to_ascii_lowercase();
        assert!(
            sent.contains("content-type: application/x-www-form-urlencoded"),
            "{sent}"
        );
        assert!(sent.ends_with("grant_type=refresh_token"), "{sent}");

        // 401 は refresh 判定のため呼び出し元へ status と JSON を返す(エラーにしない)。
        let (status, body) = post_json(&client, &url, "secret", &serde_json::json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "invalid_grant");
        let sent = requests.recv().unwrap().to_ascii_lowercase();
        assert!(sent.contains("authorization: bearer secret"), "{sent}");
        assert!(sent.contains("content-type: application/json"), "{sent}");

        // JSON でない成功応答は Null として返す。
        let (status, body) = post_form(&client, &url, &[]).await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_null());
    }

    #[test]
    fn retryable_marker_survives_anyhow_context() {
        let error = retryable_error("temporary".to_string()).context("provider fetch");
        assert!(is_retryable(&error));
        assert!(!is_retryable(&anyhow!("invalid session")));
    }

    #[test]
    fn no_retry_after_refresh_drops_the_marker_but_keeps_the_message() {
        // refresh を送った後の一時エラーは再試行に回さない(rotation 済みの refresh token を
        // もう一度送らないため)。ただし原因の文面はそのまま利用者に見せる。
        let error = retryable_error("POST token endpoint: timed out".to_string())
            .context("refreshing Grok OAuth token");
        assert!(is_retryable(&error));

        let suppressed = no_retry_after_refresh(error);
        assert!(!is_retryable(&suppressed));
        let message = format!("{suppressed:#}");
        assert!(
            message.contains("refreshing Grok OAuth token"),
            "context が失われた: {message}"
        );
        assert!(
            message.contains("POST token endpoint: timed out"),
            "原因が失われた: {message}"
        );
    }

    #[test]
    fn no_retry_after_refresh_leaves_non_retryable_errors_untouched() {
        // marker の無いエラーは加工しない(auth 失敗の文面判定を壊さない)。
        let kept = no_retry_after_refresh(anyhow!("HTTP 401 from https://example.test"));
        assert!(!is_retryable(&kept));
        assert_eq!(kept.to_string(), "HTTP 401 from https://example.test");
    }

    #[test]
    fn retryable_status_covers_timeout_rate_limit_and_server_errors() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(is_retryable_status(status), "status={status}");
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            assert!(!is_retryable_status(status), "status={status}");
        }
    }
}
