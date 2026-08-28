//! Import a ChatGPT web session from the user's local Chrome profile.
//!
//! Only the short-lived exchange happens here. Raw cookies never cross the
//! Tauri boundary and are not written to logs or returned to the frontend.

use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

use crate::proxy::providers::codex_oauth_auth::extract_identity_from_access_token;

const CHATGPT_ORIGIN: &str = "https://chatgpt.com";
const CHATGPT_SESSION_URL: &str = "https://chatgpt.com/api/auth/session";
const SESSION_REQUEST_TIMEOUT_SECS: u64 = 15;
const CHATGPT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

/// A browser session after it has been exchanged for a ChatGPT access token.
///
/// This type is crate-local on purpose: the cookie header must not become part
/// of the frontend API.
pub(crate) struct CodexBrowserSession {
    pub(crate) access_token: String,
    pub(crate) account_id: String,
    pub(crate) email: Option<String>,
    pub(crate) expires_at_ms: Option<i64>,
    pub(crate) cookie_header: String,
}

/// Exchange a cookie copied by the user from Chrome DevTools for a Codex token.
///
/// Chrome's App-Bound Encryption deliberately prevents reliable third-party
/// decryption. The user-mediated DevTools flow lets Chrome reveal the cookie
/// value and avoids filesystem, DPAPI and CDP access entirely.
pub async fn import_from_cookie(cookie_input: &str) -> Result<CodexBrowserSession, String> {
    let cookie_header = normalize_cookie_header(cookie_input)?;
    fetch_session_with_cookie(&cookie_header).await
}

/// Revalidate a previously imported cookie session after its access token
/// expires. The header is supplied by the managed auth store, never by UI.
pub(crate) async fn fetch_session_with_cookie(
    cookie_header: &str,
) -> Result<CodexBrowserSession, String> {
    if cookie_header.trim().is_empty() {
        return Err("The Chrome session cookie is empty.".to_string());
    }

    let response = crate::proxy::http_client::get()
        .get(CHATGPT_SESSION_URL)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .header("Cookie", cookie_header)
        .header("Origin", CHATGPT_ORIGIN)
        .header("Referer", format!("{CHATGPT_ORIGIN}/"))
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header("User-Agent", CHATGPT_USER_AGENT)
        .timeout(Duration::from_secs(SESSION_REQUEST_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| format!("ChatGPT session request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(match status {
            reqwest::StatusCode::UNAUTHORIZED => {
                "The Chrome ChatGPT session has expired or is not authorized (HTTP 401)."
                    .to_string()
            }
            reqwest::StatusCode::FORBIDDEN => {
                "ChatGPT rejected the direct session request (HTTP 403).".to_string()
            }
            _ => format!("ChatGPT session request failed with HTTP {status}."),
        });
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse ChatGPT session response: {e}"))?;
    parse_session_payload(payload, cookie_header.to_string())
}

#[cfg(any())]
mod legacy_file_reader {
    fn chrome_cookie_names() -> Vec<String> {
        let mut names = vec![
            "oai-did".to_string(),
            "__cf_bm".to_string(),
            "_cfuvid".to_string(),
            "_account".to_string(),
            "__oailb".to_string(),
            "__cflb".to_string(),
            "cf_clearance".to_string(),
            "oai-mweb-route-desktop".to_string(),
        ];

        for prefix in [
            "__Secure-next-auth.session-token",
            "__Host-next-auth.session-token",
            "next-auth.session-token",
        ] {
            names.push(prefix.to_string());
            for index in 0..10 {
                names.push(format!("{prefix}.{index}"));
            }
        }

        names
    }

    fn chrome_profile_names() -> Vec<String> {
        let Some(user_data_dir) = chrome_user_data_dir() else {
            return vec!["Default".to_string()];
        };

        let mut profiles = vec!["Default".to_string()];
        let local_state_path = user_data_dir.join("Local State");
        if let Ok(content) = std::fs::read_to_string(local_state_path) {
            if let Ok(value) = serde_json::from_str::<Value>(&content) {
                if let Some(info_cache) = value
                    .get("profile")
                    .and_then(|profile| profile.get("info_cache"))
                    .and_then(Value::as_object)
                {
                    profiles.extend(
                        info_cache
                            .keys()
                            .filter(|profile| profile.as_str() != "Default")
                            .cloned(),
                    );
                }
            }
        }

        profiles.sort();
        profiles.dedup();
        profiles
    }

    fn chrome_user_data_dir() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            return dirs::data_local_dir()
                .map(|dir| dir.join("Google").join("Chrome").join("User Data"));
        }

        #[cfg(target_os = "macos")]
        {
            return dirs::data_dir().map(|dir| dir.join("Google").join("Chrome"));
        }

        #[cfg(target_os = "linux")]
        {
            return dirs::config_dir().map(|dir| dir.join("google-chrome"));
        }

        #[allow(unreachable_code)]
        None
    }

    fn build_cookie_header(cookies: &[Cookie]) -> String {
        let mut selected = BTreeMap::new();
        for cookie in cookies {
            if is_allowed_cookie_name(&cookie.name) && !cookie.value.is_empty() {
                selected.insert(cookie.name.clone(), cookie.value.clone());
            }
        }

        selected
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn is_allowed_cookie_name(name: &str) -> bool {
    name == "oai-did"
        || name == "__cf_bm"
        || name == "_cfuvid"
        || name == "_account"
        || name == "__oailb"
        || name == "__cflb"
        || name == "cf_clearance"
        || name == "oai-mweb-route-desktop"
        || name == "__Secure-next-auth.session-token"
        || name.starts_with("__Secure-next-auth.session-token.")
        || name == "__Host-next-auth.session-token"
        || name.starts_with("__Host-next-auth.session-token.")
        || name == "next-auth.session-token"
        || name.starts_with("next-auth.session-token.")
}

fn normalize_cookie_header(input: &str) -> Result<String, String> {
    if input.len() > 64 * 1024 {
        return Err("The pasted Chrome cookie data is too large.".to_string());
    }

    let mut selected = BTreeMap::new();
    for raw_part in input.replace("Cookie:", "").split([';', '\r', '\n']) {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }

        let (name, value) = part
            .split_once('=')
            .or_else(|| part.split_once('\t'))
            .ok_or_else(|| "Each pasted cookie must use name=value format.".to_string())?;
        let name = name.trim();
        let value = value.trim();
        if !is_allowed_cookie_name(name) || value.is_empty() {
            continue;
        }
        if value.chars().any(|c| c.is_control()) {
            return Err(
                "The pasted Chrome cookie contains invalid control characters.".to_string(),
            );
        }
        selected.insert(name.to_string(), value.to_string());
    }

    let has_session_cookie = selected
        .keys()
        .any(|name| name.contains("next-auth.session-token"));
    if !has_session_cookie {
        return Err(
            "No ChatGPT session cookie was found. Paste the __Secure-next-auth.session-token.0/.1 cookie rows from chatgpt.com."
                .to_string(),
        );
    }

    Ok(selected
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

#[cfg(any())]
mod legacy_cdp {
    type CdpSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

    struct CdpClient {
        socket: CdpSocket,
        next_id: u64,
    }

    impl CdpClient {
        async fn connect() -> Result<Self, String> {
            let endpoint = chrome_cdp_endpoint()?;
            let mut request = endpoint
                .into_client_request()
                .map_err(|e| format!("Invalid Chrome remote debugging endpoint: {e}"))?;
            // Chrome 136+ rejects websocket clients without an allowed Origin.
            request
                .headers_mut()
                .insert("Origin", HeaderValue::from_static("http://localhost"));
            let (socket, _) = tokio::time::timeout(
                Duration::from_secs(CDP_COMMAND_TIMEOUT_SECS),
                connect_async(request),
            )
            .await
            .map_err(|_| "Timed out connecting to Chrome remote debugging.".to_string())?
            .map_err(|e| format!("Could not connect to Chrome remote debugging: {e}"))?;

            Ok(Self { socket, next_id: 0 })
        }

        async fn command(
            &mut self,
            method: &str,
            params: Value,
            session_id: Option<&str>,
        ) -> Result<Value, String> {
            self.next_id += 1;
            let id = self.next_id;
            let mut request = serde_json::json!({
                "id": id,
                "method": method,
                "params": params,
            });
            if let Some(session_id) = session_id {
                request["sessionId"] = Value::String(session_id.to_string());
            }

            self.socket
                .send(Message::Text(request.to_string().into()))
                .await
                .map_err(|e| format!("Failed to send Chrome DevTools command: {e}"))?;

            let deadline = tokio::time::sleep(Duration::from_secs(CDP_COMMAND_TIMEOUT_SECS));
            tokio::pin!(deadline);

            loop {
                tokio::select! {
                    _ = &mut deadline => {
                        return Err(format!("Timed out waiting for Chrome DevTools command {method}."));
                    }
                    message = self.socket.next() => {
                        let Some(message) = message else {
                            return Err("Chrome remote debugging connection closed.".to_string());
                        };
                        match message.map_err(|e| format!("Failed to read Chrome DevTools response: {e}"))? {
                            Message::Text(text) => {
                                let response: Value = serde_json::from_str(&text)
                                    .map_err(|e| format!("Invalid Chrome DevTools response: {e}"))?;
                                if response.get("id").and_then(Value::as_u64) != Some(id) {
                                    continue;
                                }
                                if let Some(error) = response.get("error") {
                                    return Err(format!("Chrome DevTools command {method} failed: {error}"));
                                }
                                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                            }
                            Message::Binary(bytes) => {
                                let response: Value = serde_json::from_slice(&bytes)
                                    .map_err(|e| format!("Invalid Chrome DevTools response: {e}"))?;
                                if response.get("id").and_then(Value::as_u64) != Some(id) {
                                    continue;
                                }
                                if let Some(error) = response.get("error") {
                                    return Err(format!("Chrome DevTools command {method} failed: {error}"));
                                }
                                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                            }
                            Message::Ping(payload) => {
                                self.socket.send(Message::Pong(payload)).await
                                    .map_err(|e| format!("Failed to keep Chrome DevTools alive: {e}"))?;
                            }
                            Message::Close(_) => {
                                return Err("Chrome remote debugging connection closed.".to_string());
                            }
                            Message::Pong(_) | Message::Frame(_) => {}
                        }
                    }
                }
            }
        }
    }

    fn chrome_cdp_endpoint() -> Result<String, String> {
        let path = chrome_user_data_dir()
            .ok_or_else(|| "Chrome user data directory was not found.".to_string())?
            .join("DevToolsActivePort");
        let content = std::fs::read_to_string(&path).map_err(|_| {
        "Chrome cookie decryption is unavailable. Enable Chrome remote debugging at chrome://inspect/#remote-debugging, then retry.".to_string()
    })?;
        let (port, browser_path) = parse_devtools_active_port(&content)
            .map_err(|error| format!("Chrome remote debugging endpoint is invalid: {error}"))?;
        Ok(format!("ws://127.0.0.1:{port}{browser_path}"))
    }

    fn parse_devtools_active_port(content: &str) -> Result<(u16, String), String> {
        let mut lines = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let port = lines
            .next()
            .and_then(|line| line.parse::<u16>().ok())
            .ok_or_else(|| "port is invalid".to_string())?;
        let browser_path = lines
            .next()
            .filter(|path| path.starts_with("/devtools/browser/"))
            .ok_or_else(|| "browser websocket path is invalid".to_string())?;
        Ok((port, browser_path.to_string()))
    }

    async fn import_from_chrome_cdp() -> Result<Vec<CodexBrowserSession>, String> {
        let mut client = CdpClient::connect().await?;
        let targets = client
            .command("Target.getTargets", serde_json::json!({}), None)
            .await?
            .get("targetInfos")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "Chrome returned no debuggable targets.".to_string())?;

        let mut contexts = Vec::new();
        for target in &targets {
            if target.get("type").and_then(Value::as_str) != Some("page") {
                continue;
            }
            if let Some(context) = target.get("browserContextId").and_then(Value::as_str) {
                if !contexts.iter().any(|existing| existing == context) {
                    contexts.push(context.to_string());
                }
            }
        }

        let mut targets_to_read = Vec::new();
        for context in contexts {
            match client
                .command(
                    "Target.createTarget",
                    serde_json::json!({
                        "url": CHATGPT_ORIGIN,
                        "browserContextId": context,
                    }),
                    None,
                )
                .await
            {
                Ok(target) => {
                    if let Some(target_id) = target.get("targetId").and_then(Value::as_str) {
                        targets_to_read.push((target_id.to_string(), true));
                    }
                }
                Err(error) => {
                    log::debug!("Could not create a ChatGPT target in Chrome context: {error}");
                }
            }
        }

        if targets_to_read.is_empty() {
            let target = client
                .command(
                    "Target.createTarget",
                    serde_json::json!({"url": CHATGPT_ORIGIN}),
                    None,
                )
                .await?;
            if let Some(target_id) = target.get("targetId").and_then(Value::as_str) {
                targets_to_read.push((target_id.to_string(), true));
            }
        }

        let mut sessions = Vec::new();
        let mut seen_accounts = HashSet::new();
        let mut errors = Vec::new();
        for (target_id, close_after) in targets_to_read {
            tokio::time::sleep(Duration::from_millis(CDP_PAGE_LOAD_WAIT_MS)).await;
            let attached = client
                .command(
                    "Target.attachToTarget",
                    serde_json::json!({"targetId": target_id, "flatten": true}),
                    None,
                )
                .await;
            let session_id = match attached
                .as_ref()
                .ok()
                .and_then(|value| value.get("sessionId"))
                .and_then(Value::as_str)
            {
                Some(session_id) => session_id.to_string(),
                None => {
                    if close_after {
                        let _ = client
                            .command(
                                "Target.closeTarget",
                                serde_json::json!({"targetId": target_id}),
                                None,
                            )
                            .await;
                    }
                    continue;
                }
            };

            let cookie_result = client
                .command(
                    "Network.getCookies",
                    serde_json::json!({"urls": [CHATGPT_ORIGIN, CHATGPT_SESSION_URL]}),
                    Some(&session_id),
                )
                .await;
            let _ = client
                .command(
                    "Target.detachFromTarget",
                    serde_json::json!({"sessionId": session_id}),
                    None,
                )
                .await;
            if close_after {
                let _ = client
                    .command(
                        "Target.closeTarget",
                        serde_json::json!({"targetId": target_id}),
                        None,
                    )
                    .await;
            }

            let cookies = cookie_result?
                .get("cookies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let header = build_cookie_header_from_cdp(&cookies);
            if header.is_empty() {
                errors.push(
                    "Chrome DevTools returned no recognized ChatGPT session cookies.".to_string(),
                );
                continue;
            }
            match fetch_session_with_cdp(&mut client, &session_id, &header).await {
                Ok(session) if seen_accounts.insert(session.account_id.clone()) => {
                    sessions.push(session)
                }
                Ok(_) => {}
                Err(error) => {
                    log::debug!("Chrome CDP session was not accepted: {error}");
                    errors.push(error);
                }
            }
        }

        if sessions.is_empty() {
            return Err(errors.pop().unwrap_or_else(|| {
            "Chrome DevTools returned no valid ChatGPT session. Open chatgpt.com in the signed-in Chrome profile and retry.".to_string()
        }));
        }
        Ok(sessions)
    }

    async fn fetch_session_with_cdp(
        client: &mut CdpClient,
        session_id: &str,
        cookie_header: &str,
    ) -> Result<CodexBrowserSession, String> {
        let session_url = serde_json::to_string(CHATGPT_SESSION_URL)
            .map_err(|e| format!("Could not encode ChatGPT session URL: {e}"))?;
        let expression = format!(
        "(async()=>{{try{{const r=await fetch({session_url},{{credentials:'include',cache:'no-store',headers:{{Accept:'application/json'}}}});return JSON.stringify({{status:r.status,body:await r.text()}})}}catch(e){{return JSON.stringify({{error:String(e)}})}}}})()"
    );
        let result = client
            .command(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression,
                    "awaitPromise": true,
                    "returnByValue": true,
                }),
                Some(session_id),
            )
            .await?;
        let raw = result
            .get("result")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str)
            .ok_or_else(|| "Chrome did not return a ChatGPT session response.".to_string())?;
        let response: Value = serde_json::from_str(raw)
            .map_err(|e| format!("Invalid ChatGPT session response from Chrome: {e}"))?;
        if let Some(error) = response.get("error").and_then(Value::as_str) {
            return Err(format!(
                "Chrome could not request the ChatGPT session: {error}"
            ));
        }
        let status = response
            .get("status")
            .and_then(Value::as_u64)
            .ok_or_else(|| "ChatGPT session response did not contain a status.".to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "Chrome ChatGPT session request failed (HTTP {status})."
            ));
        }
        let payload = response
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| "ChatGPT session response body was empty.".to_string())?;
        let payload: Value = serde_json::from_str(payload)
            .map_err(|e| format!("Failed to parse ChatGPT session response: {e}"))?;
        parse_session_payload(payload, cookie_header.to_string())
    }

    fn build_cookie_header_from_cdp(cookies: &[Value]) -> String {
        let mut selected = BTreeMap::new();
        for cookie in cookies {
            let Some(name) = cookie.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(value) = cookie.get("value").and_then(Value::as_str) else {
                continue;
            };
            if is_allowed_cookie_name(name) && !value.is_empty() {
                selected.insert(name.to_string(), value.to_string());
            }
        }
        selected
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn parse_session_payload(
    payload: Value,
    cookie_header: String,
) -> Result<CodexBrowserSession, String> {
    let access_token = find_string_field(
        &payload,
        &["accessToken", "access_token", "sessionToken", "session_token"],
        0,
    )
    .ok_or_else(|| {
        "Chrome ChatGPT session did not contain an access token. Try signing in to chatgpt.com again."
            .to_string()
    })?;

    let (token_account_id, token_email, token_expires_at_ms) =
        extract_identity_from_access_token(&access_token);
    let account_id = token_account_id
        .or_else(|| find_string_field(&payload, &["chatgpt_account_id", "chatgptAccountId"], 0));
    let account_id = account_id.ok_or_else(|| {
        "Could not determine the ChatGPT account ID from the Chrome session.".to_string()
    })?;

    let email = find_string_field(&payload, &["email"], 0).or(token_email);
    let expires_at_ms = find_session_expiry(&payload).or(token_expires_at_ms);

    if expires_at_ms.is_some_and(|expires_at| expires_at <= chrono::Utc::now().timestamp_millis()) {
        return Err("The Chrome ChatGPT session has expired. Sign in again and retry.".to_string());
    }

    Ok(CodexBrowserSession {
        access_token,
        account_id,
        email,
        expires_at_ms,
        cookie_header,
    })
}

fn find_string_field(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth > 3 {
        return None;
    }

    let object = value.as_object()?;
    for key in keys {
        if let Some(candidate) = object.get(*key).and_then(Value::as_str) {
            let candidate = candidate.trim();
            if !candidate.is_empty() {
                return Some(candidate.to_string());
            }
        }
    }

    for key in ["session", "tokens", "data", "user", "account"] {
        if let Some(nested) = object.get(key) {
            if let Some(value) = find_string_field(nested, keys, depth + 1) {
                return Some(value);
            }
        }
    }

    None
}

fn find_session_expiry(value: &Value) -> Option<i64> {
    let object = value.as_object()?;
    for key in ["expires", "expiresAt", "expires_at"] {
        if let Some(value) = object.get(key) {
            if let Some(expiry) = parse_expiry_value(value) {
                return Some(expiry);
            }
        }
    }

    for key in ["session", "data"] {
        if let Some(nested) = object.get(key) {
            if let Some(expiry) = find_session_expiry(nested) {
                return Some(expiry);
            }
        }
    }

    None
}

fn parse_expiry_value(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(if number < 10_000_000_000 {
            number.saturating_mul(1000)
        } else {
            number
        });
    }

    let text = value.as_str()?.trim();
    if let Ok(number) = text.parse::<i64>() {
        return Some(if number < 10_000_000_000 {
            number.saturating_mul(1000)
        } else {
            number
        });
    }

    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|date| date.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    #[test]
    fn normalize_cookie_header_accepts_cookie_lines_and_filters_unrelated_values() {
        let header = normalize_cookie_header(
            "Cookie: __Secure-next-auth.session-token.0=part-a\n__Secure-next-auth.session-token.1=part-b\nunrelated=value",
        )
        .unwrap();

        assert!(header.contains("__Secure-next-auth.session-token.0=part-a"));
        assert!(header.contains("__Secure-next-auth.session-token.1=part-b"));
        assert!(!header.contains("unrelated"));
    }

    #[test]
    fn normalize_cookie_header_requires_a_chatgpt_session_cookie() {
        let error = normalize_cookie_header("_account=account-1").unwrap_err();
        assert!(error.contains("No ChatGPT session cookie"));
    }

    #[test]
    fn parse_expiry_accepts_seconds_milliseconds_and_iso() {
        assert_eq!(
            parse_expiry_value(&Value::from(1_700_000_000_i64)),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            parse_expiry_value(&Value::from(1_700_000_000_000_i64)),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            parse_expiry_value(&Value::from("2023-11-14T22:13:20Z")),
            Some(1_700_000_000_000)
        );
    }

    #[test]
    fn find_string_field_reads_nested_session_payload() {
        let payload = serde_json::json!({
            "session": {
                "user": { "email": "user@example.com" }
            }
        });

        assert_eq!(
            find_string_field(&payload, &["email"], 0).as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn parse_session_payload_extracts_token_identity_and_expiry() {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = URL_SAFE_NO_PAD.encode(
            b"{\"chatgpt_account_id\":\"acc-123\",\"email\":\"user@example.com\",\"exp\":4102444800}",
        );
        let token = format!("{header}.{payload}.");
        let session = parse_session_payload(
            serde_json::json!({
                "accessToken": token,
                "expires": "2099-01-01T00:00:00Z",
                "user": { "email": "user@example.com" }
            }),
            "session-cookie=value".to_string(),
        )
        .unwrap();

        assert_eq!(session.account_id, "acc-123");
        assert_eq!(session.email.as_deref(), Some("user@example.com"));
        assert!(session.expires_at_ms.unwrap() > chrono::Utc::now().timestamp_millis());
        assert_eq!(session.cookie_header, "session-cookie=value");
    }
}
