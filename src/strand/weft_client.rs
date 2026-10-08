//! LOOM's token-authenticated worker API used by the Weft strand.

use std::fmt;
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::LoomConfig;

const CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoomTurn {
    pub id: String,
    pub strand: String,
    pub target_kind: String,
    pub target_id: String,
    pub queued_at: String,
    pub attempt: u32,
    pub contract_version: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LoomBrief {
    pub contract_version: u32,
    #[serde(default)]
    pub turn: Value,
    #[serde(default)]
    pub limits: Value,
    #[serde(default)]
    pub model_hint: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub scope: Value,
    #[serde(default)]
    pub envelope: Value,
    #[serde(default)]
    pub output: Value,
    #[serde(default)]
    pub attachments: Vec<Value>,
    #[serde(default)]
    pub prompt_markdown: String,
    #[serde(default)]
    pub handoff: Option<Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedTurn {
    pub id: String,
    pub lease_expires_at: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Heartbeat {
    pub lease_expires_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailResponse {
    pub requeued: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub claimed_by_worker: Option<String>,
}

#[derive(Debug)]
pub enum ClaimOutcome {
    Claimed(ClaimedTurn),
    /// The previous claim committed for this same worker, but its response was lost.
    AlreadyClaimed,
    /// Another worker won the claim race; this is ordinary queue contention.
    Contended,
}

#[derive(Debug)]
pub enum LoomError {
    Unauthorized,
    Http {
        status: u16,
        code: String,
        message: String,
        claimed_by_worker: Option<String>,
    },
    LeaseLost,
    Transport(String),
    Decode(String),
    ContractUnsupported(u32),
    Token(std::io::Error),
}

impl fmt::Display for LoomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("LOOM rejected the configured token"),
            Self::Http {
                status,
                code,
                message,
                ..
            } => {
                write!(f, "LOOM HTTP {status} ({code}): {message}")
            }
            Self::LeaseLost => f.write_str("LOOM lease was lost"),
            Self::Transport(message) => write!(f, "LOOM transport error: {message}"),
            Self::Decode(message) => write!(f, "invalid LOOM response: {message}"),
            Self::ContractUnsupported(version) => {
                write!(f, "unsupported LOOM contract version {version}")
            }
            Self::Token(error) => write!(f, "cannot read LOOM token file: {error}"),
        }
    }
}

impl std::error::Error for LoomError {}

/// Synchronous ureq client. Calls are bounded by ureq's timeout and retry
/// transient connection/5xx failures with the contract's 1s/5s/30s schedule.
pub struct LoomClient {
    config: LoomConfig,
    worker_name: String,
    agent: ureq::Agent,
    retry_delays: Vec<Duration>,
    rejected_token: std::sync::Mutex<Option<String>>,
}

impl LoomClient {
    pub fn new(config: LoomConfig, worker_name: impl Into<String>) -> Self {
        Self {
            config,
            worker_name: worker_name.into(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(20))
                .build(),
            retry_delays: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            rejected_token: std::sync::Mutex::new(None),
        }
    }

    pub fn list_turns(&self, strands: &[String]) -> Result<Vec<LoomTurn>, LoomError> {
        if strands.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!(
            "{}/api/v1/turns?status=queued&strand={}&limit=10",
            self.config.base_url.trim_end_matches('/'),
            url_encode(&strands.join(","))
        );
        self.request("GET", &url, None::<&Value>)
    }

    pub fn claim(&self, turn_id: &str) -> Result<ClaimOutcome, LoomError> {
        let url = self.endpoint(turn_id, "claim");
        let body = serde_json::json!({ "lease_secs": self.config.lease_secs });
        match self.request("POST", &url, Some(&body)) {
            Ok(claimed) => Ok(ClaimOutcome::Claimed(claimed)),
            Err(LoomError::Http {
                status: 409,
                code,
                claimed_by_worker,
                ..
            }) => {
                if code == "already_claimed"
                    && claimed_by_worker.as_deref() == Some(self.worker_name.as_str())
                {
                    Ok(ClaimOutcome::AlreadyClaimed)
                } else {
                    Ok(ClaimOutcome::Contended)
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn brief(&self, turn_id: &str) -> Result<LoomBrief, LoomError> {
        let brief: LoomBrief =
            self.request("GET", &self.endpoint(turn_id, "brief"), None::<&Value>)?;
        if brief.contract_version != CONTRACT_VERSION {
            let error = LoomError::ContractUnsupported(brief.contract_version);
            let _ = self.fail(turn_id, "contract_unsupported", &error.to_string(), false);
            return Err(error);
        }
        Ok(brief)
    }

    pub fn heartbeat(&self, turn_id: &str) -> Result<Heartbeat, LoomError> {
        self.request(
            "POST",
            &self.endpoint(turn_id, "heartbeat"),
            Some(&serde_json::json!({})),
        )
    }

    pub fn complete(&self, turn_id: &str, completion: &Value) -> Result<Value, LoomError> {
        self.request(
            "POST",
            &self.endpoint(turn_id, "complete"),
            Some(completion),
        )
    }

    pub fn fail(
        &self,
        turn_id: &str,
        error_code: &str,
        detail: &str,
        retryable: bool,
    ) -> Result<FailResponse, LoomError> {
        let body = serde_json::json!({
            "error_code": error_code,
            "detail": detail,
            "retryable": retryable,
        });
        self.request("POST", &self.endpoint(turn_id, "fail"), Some(&body))
    }

    fn endpoint(&self, turn_id: &str, suffix: &str) -> String {
        format!(
            "{}/api/v1/turns/{}/{}",
            self.config.base_url.trim_end_matches('/'),
            url_encode(turn_id),
            suffix
        )
    }

    fn token(&self) -> Result<String, LoomError> {
        let token = self.config.read_token().map_err(LoomError::Token)?;
        let rejected = self.rejected_token.lock().expect("token mutex poisoned");
        if rejected.as_deref() == Some(token.as_str()) {
            return Err(LoomError::Unauthorized);
        }
        Ok(token)
    }

    fn request<T: DeserializeOwned>(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
    ) -> Result<T, LoomError> {
        let token = self.token()?;
        let mut last_error = None;
        for attempt in 0..=self.retry_delays.len() {
            let mut request = self
                .agent
                .request(method, url)
                .set("Authorization", &format!("Bearer {token}"))
                .set("X-Loom-Worker", &self.worker_name)
                .set("Accept", "application/json");
            let response = if let Some(body) = body {
                request = request.set("Content-Type", "application/json");
                request.send_string(
                    &serde_json::to_string(body)
                        .map_err(|error| LoomError::Decode(error.to_string()))?,
                )
            } else {
                request.call()
            };
            match response {
                Ok(response) => {
                    let body = response
                        .into_string()
                        .map_err(|error| LoomError::Transport(error.to_string()))?;
                    return serde_json::from_str(&body)
                        .map_err(|error| LoomError::Decode(error.to_string()));
                }
                Err(ureq::Error::Status(status, response)) => {
                    let api_error = response
                        .into_string()
                        .ok()
                        .and_then(|body| serde_json::from_str::<ApiError>(&body).ok())
                        .unwrap_or(ApiError {
                            code: String::new(),
                            message: String::new(),
                            claimed_by_worker: None,
                        });
                    if status == 401 {
                        *self.rejected_token.lock().expect("token mutex poisoned") =
                            Some(token.clone());
                        tracing::warn!(event = "weft.token_rejected", worker = %self.worker_name, "LOOM rejected worker token; Weft disabled until token file changes");
                        return Err(LoomError::Unauthorized);
                    }
                    if status == 409 && api_error.code == "lease_lost" {
                        return Err(LoomError::LeaseLost);
                    }
                    if status >= 500 {
                        last_error = Some(LoomError::Http {
                            status,
                            code: api_error.code,
                            message: api_error.message,
                            claimed_by_worker: api_error.claimed_by_worker,
                        });
                    } else {
                        return Err(LoomError::Http {
                            status,
                            code: api_error.code,
                            message: api_error.message,
                            claimed_by_worker: api_error.claimed_by_worker,
                        });
                    }
                }
                Err(ureq::Error::Transport(error)) => {
                    last_error = Some(LoomError::Transport(error.to_string()));
                }
            }
            if let Some(delay) = self.retry_delays.get(attempt) {
                thread::sleep(*delay);
            }
        }
        Err(last_error.unwrap_or_else(|| LoomError::Transport("request failed".into())))
    }
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~,".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::thread;

    fn serve(responses: Vec<(u16, &'static str)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 2048];
                loop {
                    let size = stream.read(&mut chunk).unwrap();
                    if size == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..size]);
                    let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).to_string());
                let reason = if status == 200 {
                    "OK"
                } else if status == 401 {
                    "Unauthorized"
                } else {
                    "Conflict"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ).unwrap();
            }
            requests
        });
        (address, handle)
    }

    fn make_client(base_url: String) -> (tempfile::TempDir, LoomClient, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("token");
        write_token(&token_file, "secret-one");
        let config = LoomConfig {
            base_url,
            token_file: token_file.clone(),
            ..LoomConfig::default()
        };
        (
            directory,
            LoomClient::new(config, "weft-worker"),
            token_file,
        )
    }

    fn write_token(path: &std::path::Path, token: &str) {
        std::fs::write(path, token).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn request_uses_worker_auth_query_and_accept_headers() {
        let (url, server) = serve(vec![(200, "[]")]);
        let (_directory, client, _) = make_client(url);
        client
            .list_turns(&["advisor".into(), "courier".into()])
            .unwrap();
        let requests = server.join().unwrap();
        let request = &requests[0].to_ascii_lowercase();
        assert!(
            request.starts_with("get /api/v1/turns?status=queued&strand=advisor,courier&limit=10 ")
        );
        assert!(request.contains("authorization: bearer secret-one"));
        assert!(request.contains("x-loom-worker: weft-worker"));
        assert!(request.contains("accept: application/json"));
    }

    #[test]
    fn claim_409_is_success_only_for_same_worker() {
        let (url, server) = serve(vec![
            (
                409,
                r#"{"code":"already_claimed","claimed_by_worker":"weft-worker"}"#,
            ),
            (
                409,
                r#"{"code":"already_claimed","claimed_by_worker":"other-worker"}"#,
            ),
        ]);
        let (_directory, client, _) = make_client(url);
        assert!(matches!(
            client.claim("turn-1").unwrap(),
            ClaimOutcome::AlreadyClaimed
        ));
        assert!(matches!(
            client.claim("turn-2").unwrap(),
            ClaimOutcome::Contended
        ));
        server.join().unwrap();
    }

    #[test]
    fn unauthorized_token_is_disabled_until_token_file_changes() {
        let (url, server) = serve(vec![(401, r#"{"code":"unauthorized"}"#), (200, "[]")]);
        let (_directory, client, token_file) = make_client(url);
        assert!(matches!(
            client.list_turns(&["advisor".into()]),
            Err(LoomError::Unauthorized)
        ));
        assert!(matches!(
            client.list_turns(&["advisor".into()]),
            Err(LoomError::Unauthorized)
        ));
        write_token(&token_file, "secret-two");
        assert!(client.list_turns(&["advisor".into()]).unwrap().is_empty());
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("Bearer secret-one"));
        assert!(requests[1].contains("Bearer secret-two"));
    }

    #[test]
    fn unknown_contract_version_fails_turn_as_non_retryable() {
        let body = r#"{"contract_version":2}"#;
        let (url, server) = serve(vec![(200, body), (200, r#"{"requeued":false}"#)]);
        let (_directory, client, _) = make_client(url);
        assert!(matches!(
            client.brief("turn-x"),
            Err(LoomError::ContractUnsupported(2))
        ));
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /api/v1/turns/turn-x/brief "));
        assert!(requests[1].starts_with("POST /api/v1/turns/turn-x/fail "));
        assert!(requests[1].contains(r#""error_code":"contract_unsupported""#));
        assert!(requests[1].contains(r#""retryable":false"#));
    }
}
