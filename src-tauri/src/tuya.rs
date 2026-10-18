//! Tuya Open API (cloud, v1.0, HMAC-SHA256 simple mode), as proven live in T-007's spike:
//! a token, the linked app's device list, a light's modes and one `work_mode` command.
//! Every failure is HTTP 200 with `success:false` + `code`, so `success` is what counts.
//! Device ids, `local_key`, `ip`, `uid` and the keys never reach a log or the page.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Data center label (what Settings shows) → API host.
pub const REGIONS: &[(&str, &str)] = &[
    ("Central Europe", "openapi.tuyaeu.com"),
    ("Western Europe", "openapi-weaz.tuyaeu.com"),
    ("Western America", "openapi.tuyaus.com"),
    ("Eastern America", "openapi-ueaz.tuyaus.com"),
    ("China", "openapi.tuyacn.com"),
    ("India", "openapi.tuyain.com"),
];
const TIMEOUT: Duration = Duration::from_secs(10);
/// A token lives 2 h; use it 5 min less.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);

/// The user's own cloud project keys, kept in the Keychain. `region` is a `REGIONS` label.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Keys {
    pub access_id: String,
    pub secret: String,
    pub region: String,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keys {{ region: {:?}, .. }}", self.region)
    }
}

pub fn host(region: &str) -> Option<&'static str> {
    REGIONS.iter().find(|r| r.0 == region).map(|r| r.1)
}

#[derive(Debug, Clone, PartialEq)]
pub enum TuyaError {
    /// `success:false` with Tuya's code.
    Code(i64),
    Network,
    Timeout,
    /// Not JSON, or no `success`.
    BadReply,
}

impl TuyaError {
    pub fn code(&self) -> Option<i64> {
        match self {
            TuyaError::Code(c) => Some(*c),
            _ => None,
        }
    }
    /// The keys themselves were refused (after one fresh token).
    pub fn keys_refused(&self) -> bool {
        matches!(self.code(), Some(1004 | 1010 | 1011))
    }
    pub fn plan_expired(&self) -> bool {
        self.code() == Some(28841002)
    }
    /// The status line's reason (spec › Reasons).
    pub fn reason(&self) -> String {
        match self {
            TuyaError::Code(2001) => "offline".into(),
            TuyaError::Code(2008) => "not supported".into(),
            TuyaError::Code(1106) => "not in your Tuya project".into(),
            e if e.keys_refused() => "Tuya refused the keys".into(),
            TuyaError::Code(28841002) => "plan expired".into(),
            TuyaError::Code(28841004 | 28841104) => "monthly limit used up".into(),
            TuyaError::Code(1199 | 1110) => "Tuya was busy".into(),
            TuyaError::Network => "no connection".into(),
            TuyaError::Timeout => "no answer".into(),
            TuyaError::Code(c) => format!("error {c}"),
            TuyaError::BadReply => "error reply".into(),
        }
    }
    /// For the log: the code or the kind, nothing from the reply text.
    pub fn short(&self) -> String {
        match self {
            TuyaError::Code(c) => c.to_string(),
            TuyaError::Network => "network".into(),
            TuyaError::Timeout => "timeout".into(),
            TuyaError::BadReply => "bad reply".into(),
        }
    }
    fn retry_later(&self) -> bool {
        matches!(self, TuyaError::Network | TuyaError::Timeout | TuyaError::Code(1199 | 1110))
    }
}

/// A device of the linked app account: only what the app needs (never `local_key`, `ip`, `uid`).
#[derive(Debug, Clone, PartialEq)]
pub struct Dev {
    pub id: String,
    pub name: String,
    pub category: String,
    pub online: bool,
}

pub fn parse_devices(result: &Value) -> Result<(Vec<Dev>, Option<String>), TuyaError> {
    let list = result["devices"].as_array().ok_or(TuyaError::BadReply)?;
    let devs = list
        .iter()
        .filter_map(|d| {
            Some(Dev {
                id: d["id"].as_str()?.to_owned(),
                name: d["name"].as_str().unwrap_or_default().to_owned(),
                category: d["category"].as_str().unwrap_or_default().to_owned(),
                online: d["online"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    let next = result["has_more"].as_bool().unwrap_or(false).then(|| result["last_row_key"].as_str().map(str::to_owned)).flatten();
    Ok((devs, next))
}

/// One thing a device can be told: a writable function's code and a value for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub code: String,
    pub value: Value,
}

/// A power switch's code: `switch`, `switch_led`, `switch_fan` or a gang `switch_<n>`.
fn is_switch(code: &str) -> bool {
    matches!(code, "switch" | "switch_led" | "switch_fan") || code.strip_prefix("switch_").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The choices a device offers, from `/functions`, in Tuya's order (spec › Choices): a light
/// (an Enum `work_mode`) gets its power On / Off (`switch_led`) and its modes; anything else its
/// power switches On / Off; nothing else (no speeds, numbers or JSON). Types come in any case
/// ("Boolean", "BOOLEAN") and some IR devices list a code twice, so both are evened out.
pub fn parse_choices(result: &Value) -> Vec<Action> {
    let mut out: Vec<Action> = Vec::new();
    for f in result["functions"].as_array().into_iter().flatten() {
        let Some(code) = f["code"].as_str() else { continue };
        let ty = f["type"].as_str().unwrap_or_default().to_ascii_lowercase();
        let values: Vec<Value> = match ty.as_str() {
            "boolean" | "bool" => vec![Value::Bool(true), Value::Bool(false)],
            "enum" => serde_json::from_str::<Value>(f["values"].as_str().unwrap_or("{}")).ok().and_then(|v| v["range"].as_array().cloned()).unwrap_or_default(),
            _ => continue,
        };
        for value in values {
            let a = Action { code: code.to_owned(), value };
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    let light = out.iter().any(|a| a.code == "work_mode" && a.value.is_string());
    out.retain(|a| match (&a.value, light) {
        (Value::String(_), true) => a.code == "work_mode",
        (Value::Bool(_), true) => a.code == "switch_led",
        (Value::Bool(_), false) => is_switch(&a.code),
        _ => false,
    });
    out
}

/// What the user sees for one of a device's `choices`: a mode by its name (`Music`), a switch
/// as `On` / `Off`, or by its gang (`Switch 1 On`) when the device has more than one switch.
pub fn choice_label(choices: &[Action], a: &Action) -> String {
    match &a.value {
        Value::Bool(b) => {
            let s = if *b { "On" } else { "Off" };
            let gangs = choices.iter().filter(|c| c.value == Value::Bool(true) && is_switch(&c.code)).count() > 1;
            if gangs { format!("{} {s}", label(&a.code)) } else { s.into() }
        }
        Value::String(v) => label(v),
        v => v.to_string(),
    }
}

/// What the user sees for a mode value: `colour` → `Colour`, `scene_2` → `Scene 2`.
pub fn label(mode: &str) -> String {
    let s = mode.replace('_', " ");
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// `METHOD\nsha256(body)\n\npath` (the empty line is the Signature-Headers block), signed with
/// the secret over `head + that`; `head` = client_id + [access_token] + t + nonce.
pub fn sign(secret: &str, head: &str, method: &str, body: &str, path: &str) -> String {
    let to_sign = format!("{method}\n{}\n\n{path}", sha256_hex(body.as_bytes()));
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(format!("{head}{to_sign}").as_bytes());
    mac.finalize().into_bytes().iter().map(|x| format!("{x:02X}")).collect()
}

pub struct Client {
    base: String,
    id: String,
    secret: String,
    http: reqwest::Client,
    /// One fetch at a time: a second grant_type=1 call kills the first token (1010).
    token: tokio::sync::Mutex<Option<(String, Instant)>>,
    nonce: AtomicU64,
    /// Before the one retry after 1199 / 1110 / network.
    pub retry_wait: Duration,
}

impl Client {
    pub fn new(keys: &Keys) -> Option<Client> {
        Some(Client::with_base(format!("https://{}", host(&keys.region)?), keys))
    }

    pub fn with_base(base: String, keys: &Keys) -> Client {
        let http = reqwest::Client::builder().timeout(TIMEOUT).build().expect("reqwest client");
        Client {
            base,
            id: keys.access_id.clone(),
            secret: keys.secret.clone(),
            http,
            token: tokio::sync::Mutex::new(None),
            nonce: AtomicU64::new(0),
            retry_wait: Duration::from_secs(3),
        }
    }

    /// GET /v1.0/token?grant_type=1, never the refresh endpoint (it kills the token in use).
    pub async fn token(&self) -> Result<String, TuyaError> {
        self.token_unless(None).await
    }

    /// The cached token, or a new one. `stale`: the token a call just got 1010 / 1011 with;
    /// fetch anew only if no one else has replaced it meanwhile.
    async fn token_unless(&self, stale: Option<&str>) -> Result<String, TuyaError> {
        let mut t = self.token.lock().await;
        if let Some((tok, until)) = t.as_ref() {
            if Instant::now() < *until && Some(tok.as_str()) != stale {
                return Ok(tok.clone());
            }
        }
        *t = None;
        let r = self.raw("GET", "/v1.0/token?grant_type=1", None, "").await?;
        let tok = r["access_token"].as_str().ok_or(TuyaError::BadReply)?.to_owned();
        let life = Duration::from_secs(r["expire_time"].as_u64().unwrap_or(7200)).saturating_sub(TOKEN_MARGIN);
        *t = Some((tok.clone(), Instant::now() + life));
        Ok(tok)
    }

    /// One signed call; `result` on success. `path` carries its query with sorted keys.
    async fn raw(&self, method: &str, path: &str, body: Option<&Value>, token: &str) -> Result<Value, TuyaError> {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).to_string();
        let nonce = format!("{t}{:x}", self.nonce.fetch_add(1, Ordering::Relaxed));
        let body = body.map(Value::to_string).unwrap_or_default();
        let sig = sign(&self.secret, &format!("{}{token}{t}{nonce}", self.id), method, &body, path);
        let mut req = self
            .http
            .request(method.parse().map_err(|_| TuyaError::BadReply)?, format!("{}{path}", self.base))
            .header("client_id", &self.id)
            .header("sign", sig)
            .header("t", &t)
            .header("nonce", nonce)
            .header("sign_method", "HMAC-SHA256");
        if !token.is_empty() {
            req = req.header("access_token", token);
        }
        if !body.is_empty() {
            req = req.header("Content-Type", "application/json").body(body);
        }
        let resp = req.send().await.map_err(|e| if e.is_timeout() { TuyaError::Timeout } else { TuyaError::Network })?;
        let v: Value = resp.json().await.map_err(|e| if e.is_timeout() { TuyaError::Timeout } else { TuyaError::BadReply })?;
        match v["success"].as_bool() {
            Some(true) => Ok(v["result"].clone()),
            Some(false) => Err(TuyaError::Code(v["code"].as_i64().unwrap_or(0))),
            None => Err(TuyaError::BadReply),
        }
    }

    /// A call with the spec's retries: 1010 / 1011 → a fresh token and one retry;
    /// 1199 / 1110 / network / timeout → one retry after `retry_wait`.
    pub async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, TuyaError> {
        let (mut token_retry, mut busy_retry) = (true, true);
        let mut tok = self.token().await?;
        loop {
            match self.raw(method, path, body, &tok).await {
                Err(TuyaError::Code(1010 | 1011)) if token_retry => {
                    token_retry = false;
                    tok = self.token_unless(Some(&tok)).await?;
                }
                Err(e) if busy_retry && e.retry_later() => {
                    busy_retry = false;
                    tokio::time::sleep(self.retry_wait).await;
                }
                r => return r,
            }
        }
    }

    /// Every device of the linked app account, page by page.
    pub async fn devices(&self) -> Result<Vec<Dev>, TuyaError> {
        let mut all = Vec::new();
        let mut row: Option<String> = None;
        loop {
            // Query keys sorted: they are part of the signed path.
            let path = match &row {
                Some(k) => format!("/v1.0/iot-01/associated-users/devices?last_row_key={k}&size=50"),
                None => "/v1.0/iot-01/associated-users/devices?size=50".to_owned(),
            };
            let (devs, next) = parse_devices(&self.call("GET", &path, None).await?)?;
            all.extend(devs);
            match next {
                Some(k) if Some(&k) != row.as_ref() => row = Some(k),
                _ => return Ok(all),
            }
        }
    }

    /// The device's choices (empty: nothing it can be told).
    pub async fn choices(&self, id: &str) -> Result<Vec<Action>, TuyaError> {
        Ok(parse_choices(&self.call("GET", &format!("/v1.0/devices/{id}/functions"), None).await?))
    }

    pub async fn send(&self, id: &str, a: &Action) -> Result<(), TuyaError> {
        let body = json!({ "commands": [{ "code": a.code, "value": a.value }] });
        self.call("POST", &format!("/v1.0/devices/{id}/commands"), Some(&body)).await.map(drop)
    }
}

#[cfg(test)]
pub mod stub {
    //! A tiny local HTTP server that answers like Tuya, for the client and automation tests.
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug, Clone)]
    pub struct Req {
        pub method: String,
        pub path: String,
        pub headers: HashMap<String, String>,
        pub body: String,
    }

    /// The answer: delay in ms (u64::MAX = never), then the JSON body.
    pub type Answer = (u64, String);
    pub type Log = Arc<Mutex<Vec<Req>>>;

    pub async fn start(answer: impl Fn(&Req, usize) -> Answer + Send + Sync + 'static) -> (String, Log) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let log: Log = Arc::default();
        let (answer, log2) = (Arc::new(answer), log.clone());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let (answer, log) = (answer.clone(), log2.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    loop {
                        // Read one request: head, then Content-Length bytes.
                        let head_end = loop {
                            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break i + 4;
                            }
                            let mut chunk = [0u8; 4096];
                            match s.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };
                        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                        let mut lines = head.split("\r\n");
                        let mut first = lines.next().unwrap_or_default().split(' ');
                        let (method, path) = (first.next().unwrap_or_default().to_owned(), first.next().unwrap_or_default().to_owned());
                        let headers: HashMap<String, String> =
                            lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_ascii_lowercase(), v.to_owned())).collect();
                        let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                        while buf.len() < head_end + len {
                            let mut chunk = [0u8; 4096];
                            match s.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned();
                        buf.drain(..head_end + len);
                        let req = Req { method, path, headers, body };
                        let n = {
                            let mut l = log.lock().unwrap();
                            l.push(req.clone());
                            l.len() - 1
                        };
                        let (delay, json) = answer(&req, n);
                        if delay == u64::MAX {
                            std::future::pending::<()>().await;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}", json.len());
                        if s.write_all(resp.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (base, log)
    }

    pub fn ok(result: &str) -> String {
        format!(r#"{{"success":true,"result":{result}}}"#)
    }
    pub fn fail(code: i64) -> String {
        format!(r#"{{"success":false,"code":{code},"msg":"x"}}"#)
    }
    pub fn token(t: &str) -> String {
        ok(&format!(r#"{{"access_token":"{t}","expire_time":7200,"refresh_token":"r","uid":"u"}}"#))
    }
}

#[cfg(test)]
mod tests {
    use super::stub::{self, fail, ok, token};
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> Value {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tuya").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }
    fn keys() -> Keys {
        Keys { access_id: "id".into(), secret: "secret".into(), region: "Central Europe".into() }
    }

    #[test]
    fn mode_label_rule() {
        assert_eq!(label("scene_2"), "Scene 2");
        assert_eq!(label("colour"), "Colour");
        assert_eq!(label("music"), "Music");
        assert_eq!(label(""), "");
    }

    #[test]
    fn device_list_keeps_only_safe_fields() {
        let (devs, next) = parse_devices(&fixture("associated-users-devices.json")["result"]).unwrap();
        assert_eq!(devs, vec![Dev { id: "<device_id>".into(), name: "Table LED".into(), category: "dd".into(), online: true }]);
        assert_eq!(next, None);
        // Dev has no field for them, so nothing else can leak: check the Debug text too.
        let dbg = format!("{devs:?}");
        for k in ["local_key", "ip", "uid", "<redacted>"] {
            assert!(!dbg.contains(k), "{k}");
        }
    }

    fn act(code: &str, value: Value) -> Action {
        Action { code: code.into(), value }
    }

    #[test]
    fn choices_power_and_modes_only() {
        let labels = |c: &[Action]| c.iter().map(|a| choice_label(c, a)).collect::<Vec<_>>();
        // The Table LED sample: its power switch and its four modes; numbers and JSON left out.
        let c = parse_choices(&fixture("functions.json")["result"]);
        assert_eq!(labels(&c), ["On", "Off", "White", "Colour", "Scene", "Music"]);
        assert_eq!(c[5], act("work_mode", json!("music")));
        // A fan switch (fskg, read live): On / Off only, no speed levels, no countdown.
        let fan = json!({"functions":[
            {"code":"switch_fan","type":"Boolean","values":"{}"},
            {"code":"countdown_fan","type":"Integer","values":"{\"min\":0}"},
            {"code":"fan_speed_enum","type":"Enum","values":"{\"range\":[\"level_1\",\"level_2\"]}"}]});
        assert_eq!(labels(&parse_choices(&fan)), ["On", "Off"]);
        // A 2-gang switch: each gang its own pair; the backlight and child lock are not offered.
        let gang = json!({"functions":[
            {"code":"switch_1","type":"Boolean","values":"{}"},{"code":"switch_2","type":"Boolean","values":"{}"},
            {"code":"switch_backlight","type":"Boolean","values":"{}"},{"code":"child_lock","type":"Boolean","values":"{}"},
            {"code":"relay_status","type":"Enum","values":"{\"range\":[\"off\",\"on\"]}"}]});
        assert_eq!(labels(&parse_choices(&gang)), ["Switch 1 On", "Switch 1 Off", "Switch 2 On", "Switch 2 Off"]);
        // An IR fan (read live): upper-case types and each code listed three times.
        let ir = json!({"functions":[
            {"code":"switch","type":"BOOLEAN","values":"{}"},{"code":"switch","type":"BOOLEAN","values":"{}"},
            {"code":"option","type":"ENUM","values":"{\"range\":[\"swing\"]}"},{"code":"option","type":"ENUM","values":"{\"range\":[\"swing\"]}"}]});
        assert_eq!(parse_choices(&ir), [act("switch", json!(true)), act("switch", json!(false))]);
        // A sensor: nothing to switch. (2009 on /functions: automation's add_device_with_nothing_to_switch.)
        let sensor = json!({"functions":[{"code":"doorcontact_state","type":"Boolean","values":"{}"},{"code":"countdown","type":"Integer","values":"{}"}]});
        assert!(parse_choices(&sensor).is_empty());
    }

    #[test]
    fn offline_sample_is_2001() {
        let v = fixture("offline-command.json");
        assert_eq!((v["success"].as_bool(), v["code"].as_i64()), (Some(false), Some(2001)));
        assert_eq!(TuyaError::Code(2001).reason(), "offline");
    }

    /// Same inputs through `openssl dgst -sha256 -hmac secret` (recipe's string to sign).
    #[test]
    fn sign_matches_recipe() {
        let s = sign("secret", "id1700000000000n", "GET", "", "/v1.0/token?grant_type=1");
        assert_eq!(s, "8F7A66CF793915E478AD21EC3812F7EDBAB2054ACB8057D303E4CC66CA1F3727");
    }

    #[tokio::test]
    async fn token_refetched_once_on_1010() {
        // Token 1, the command says 1010, token 2, the command works.
        let (base, log) = stub::start(|r, n| {
            (0, match (r.path.starts_with("/v1.0/token"), n) {
                (true, 0) => token("t1"),
                (true, _) => token("t2"),
                (false, 1) => fail(1010),
                (false, _) => ok("true"),
            })
        })
        .await;
        let c = Client::with_base(base, &keys());
        c.send("dev-1", &act("work_mode", json!("music"))).await.unwrap();
        let l = log.lock().unwrap();
        let paths: Vec<_> = l.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/v1.0/token?grant_type=1", "/v1.0/devices/dev-1/commands", "/v1.0/token?grant_type=1", "/v1.0/devices/dev-1/commands"]);
        assert_eq!(l[3].headers["access_token"], "t2");
        assert_eq!(l[3].body, r#"{"commands":[{"code":"work_mode","value":"music"}]}"#);
        assert!(!l[0].headers.contains_key("access_token"));
        assert_eq!(l[3].headers["sign_method"], "HMAC-SHA256");
        // Never the refresh endpoint.
        assert!(l.iter().all(|r| !r.path.starts_with("/v1.0/token/")));
    }

    #[tokio::test]
    async fn token_1010_twice_is_keys_refused() {
        let (base, log) = stub::start(|r, _| (0, if r.path.starts_with("/v1.0/token") { token("t") } else { fail(1010) })).await;
        let c = Client::with_base(base, &keys());
        let e = c.send("<device_id>", &act("work_mode", json!("music"))).await.unwrap_err();
        assert!(e.keys_refused());
        assert_eq!(e.reason(), "Tuya refused the keys");
        assert_eq!(log.lock().unwrap().len(), 4); // token, command, token, command: one retry only
    }

    #[tokio::test]
    async fn busy_retried_once_after_wait() {
        let (base, log) = stub::start(|r, _| (0, if r.path.starts_with("/v1.0/token") { token("t") } else { fail(1199) })).await;
        let mut c = Client::with_base(base, &keys());
        c.retry_wait = Duration::from_millis(10);
        assert_eq!(c.send("<device_id>", &act("work_mode", json!("music"))).await.unwrap_err().reason(), "Tuya was busy");
        assert_eq!(log.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn token_kept_and_fetched_once_for_parallel_calls() {
        let (base, log) = stub::start(|r, _| (20, if r.path.starts_with("/v1.0/token") { token("t") } else { ok("true") })).await;
        let c = Client::with_base(base, &keys());
        let m = act("work_mode", json!("music"));
        let (a, b) = tokio::join!(c.send("a", &m), c.send("b", &m));
        a.unwrap();
        b.unwrap();
        c.send("a", &act("switch_fan", json!(false))).await.unwrap();
        assert_eq!(log.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1.0/token")).count(), 1);
    }

    #[tokio::test]
    async fn device_list_pages() {
        let (base, log) = stub::start(|r, _| {
            (0, if r.path.starts_with("/v1.0/token") {
                token("t")
            } else if r.path.contains("last_row_key=k1") {
                ok(r#"{"devices":[{"id":"b","name":"B","category":"dj","online":false}],"has_more":false}"#)
            } else {
                ok(r#"{"devices":[{"id":"a","name":"A","category":"dd","online":true,"local_key":"x"}],"has_more":true,"last_row_key":"k1"}"#)
            })
        })
        .await;
        let c = Client::with_base(base, &keys());
        let d = c.devices().await.unwrap();
        assert_eq!(d.iter().map(|d| (d.name.as_str(), d.online)).collect::<Vec<_>>(), [("A", true), ("B", false)]);
        assert_eq!(log.lock().unwrap()[2].path, "/v1.0/iot-01/associated-users/devices?last_row_key=k1&size=50");
    }

    #[test]
    fn keys_debug_hides_secret() {
        let d = format!("{:?}", keys());
        assert!(!d.contains("secret") && !d.contains("\"id\""), "{d}");
    }

    /// Reads the Table LED's list entry and modes with the owner's dev keys from ../.env.
    /// Read-only: sends no command. Prints no id or key.
    /// Run: cargo test --manifest-path src-tauri/Cargo.toml tuya_live_read -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn tuya_live_read() {
        let env = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.env")).unwrap();
        let get = |k: &str| env.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned()).unwrap();
        let region = match get("TUYA_REGION").as_str() {
            "eu" => "Central Europe",
            other => panic!("region {other} not mapped"),
        };
        let c = Client::new(&Keys { access_id: get("TUYA_ACCESS_ID"), secret: get("TUYA_ACCESS_SECRET"), region: region.into() }).unwrap();
        let t0 = Instant::now();
        let devs = c.devices().await.unwrap_or_else(|e| panic!("devices: {e:?}"));
        println!("{} devices ({:.1} s)", devs.len(), t0.elapsed().as_secs_f32());
        let led = devs.iter().find(|d| d.name == "Table LED").expect("Table LED in the list");
        println!("Table LED: category {}, online {}", led.category, led.online);
        let choices = c.choices(&led.id).await.unwrap_or_else(|e| panic!("choices: {e:?}"));
        println!("choices: {:?}", choices.iter().map(|a| choice_label(&choices, a)).collect::<Vec<_>>());
        // The same keys on another data center: the token works, the list says 28841107
        // (Connect's wrong-data-center line).
        let k = Keys { access_id: get("TUYA_ACCESS_ID"), secret: get("TUYA_ACCESS_SECRET"), region: "Western America".into() };
        let us = Client::new(&k).unwrap();
        println!("token on Western America: {:?}", us.token().await.map(drop));
        println!("list on Western America: {:?}", us.devices().await.map(|d| d.len()));
        // Read-only: no command is ever sent from here.
        // A fan switch (category fskg): its choices for the generic automation.
        for d in devs.iter().filter(|d| d.category == "fskg") {
            let ch = c.choices(&d.id).await.unwrap_or_else(|e| panic!("fan choices: {e:?}"));
            println!("fan switch (online {}): {:?}", d.online, ch.iter().map(|a| format!("{} = {}", a.code, choice_label(&ch, a))).collect::<Vec<_>>());
        }
    }
}
