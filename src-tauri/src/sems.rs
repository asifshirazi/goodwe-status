//! Everything the app knows about GoodWe SEMS lives here: URLs, headers, both logins,
//! JSON field paths and the per-reading grid signal. The rest of the app sees only
//! `Reading` and `ChartSeries`. When GoodWe changes their API, fix this file and add the
//! new reply under src-tauri/tests/fixtures/ (the tests parse every sample).
//!
//! Two APIs:
//! - classic (`www.semsportal.com`): login + live reading (it has the grid voltage).
//! - SEMS+ web gateway (`semsplus.goodwe.com`): its own login and a signed `X-Signature`
//!   header; gives today's per-minute SOC and battery kW for the chart.

use crate::clock::LocalTs;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use md5::Md5;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLASSIC_LOGIN: &str = "https://www.semsportal.com/api/v2/Common/CrossLogin";
const CLASSIC_TOKEN0: &str = r#"{"version":"v2.1.0","client":"ios","language":"en"}"#;
const PLUS_LOGIN: &str = "https://semsplus.goodwe.com/web/sems/sems-user/api/v1/auth/cross-login";
const PLUS_ORIGIN: &str = "https://semsplus.goodwe.com";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
/// Below this the grid counts as off (design doc § Rules).
const GRID_OFF_VOLTS: f64 = 50.0;

// ---------- what the rest of the app sees ----------

/// One live reading, already free of SEMS field names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    /// `info.time`, station-local.
    pub time: LocalTs,
    pub grid: GridSignal,
    /// Raw grid values, kept so a disagreement can be logged and the rule checked later.
    pub vac1: f64,
    pub grid_status: Option<String>,
    /// `None` for a station without a battery.
    pub battery: Option<Battery>,
    /// Station's offset from UTC in hours (from `info.time_span`, which SEMS sends negated).
    pub utc_offset_h: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Battery {
    pub soc: f64,
    /// Watts, positive = discharging, negative = charging (same sign as SEMS+ `pBat`).
    pub watts: f64,
    pub capacity_kwh: Option<f64>,
}

/// What one reading says about the grid, before the previous state is taken into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GridSignal {
    On,
    Off,
    /// Voltage and status disagree: the caller keeps the previous state.
    Disagree,
}

/// Today's series for the chart.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartSeries {
    /// The station-local day the points belong to (`LocalTs::day`).
    pub day: i64,
    /// (minute of day, soc %, battery kW; kW > 0 = discharging), sorted by minute.
    pub points: Vec<(u32, f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SemsError {
    /// SEMS refused the email or password. Show the sign-in box.
    WrongLogin(String),
    /// No answer, a timeout, HTTP 5xx, a rate limit: count as a failed poll.
    Network(String),
    /// The session token stopped working: log in again once.
    Expired,
    /// The reply came but could not be read. `reply` is the raw body, for the redacted dump.
    Parse { reason: String, reply: String },
    /// No stored credentials.
    SignedOut,
}

impl std::fmt::Display for SemsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemsError::WrongLogin(m) => write!(f, "login refused: {m}"),
            SemsError::Network(m) => write!(f, "network: {m}"),
            SemsError::Expired => write!(f, "session expired"),
            SemsError::Parse { reason, .. } => write!(f, "unreadable reply: {reason}"),
            SemsError::SignedOut => write!(f, "not signed in"),
        }
    }
}

// ---------- parsing (pure, tested against the samples) ----------

/// Accepts a JSON number or a numeric string ("86", "86.0", "86%").
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
}

fn missing(field: &str) -> String {
    format!("field `{field}` missing")
}

/// The grid rule for one reading: off = vac1 < 50 V and the status says off_grid;
/// on = vac1 >= 50 V and it doesn't; no status field = voltage alone.
pub fn grid_signal(vac1: f64, status: Option<&str>) -> GridSignal {
    let low = vac1 < GRID_OFF_VOLTS;
    match (low, status.map(|s| s.contains("off_grid"))) {
        (true, Some(true) | None) => GridSignal::Off,
        (false, Some(false) | None) => GridSignal::On,
        _ => GridSignal::Disagree,
    }
}

/// `v1/PowerStation/GetMonitorDetailByPowerstationId` reply → `Reading`.
pub fn parse_reading(v: &Value) -> Result<Reading, String> {
    check_classic_code(v)?;
    let data = &v["data"];
    let info = &data["info"];
    let time_s = info["time"].as_str().ok_or_else(|| missing("info.time"))?;
    let time = LocalTs::parse(time_s).ok_or_else(|| format!("field `info.time` unreadable: {time_s:?}"))?;
    let inv = data["inverter"].get(0).ok_or_else(|| missing("inverter[0]"))?;
    let full = &inv["invert_full"];
    let vac1 = num(&full["vac1"]).ok_or_else(|| missing("invert_full.vac1"))?;
    let grid_status = full["grid_conn_status"].as_str().map(str::to_owned);

    let soc = num(&full["soc"]);
    let stored = info["is_stored"].as_bool().unwrap_or(soc.is_some());
    let battery = match (stored, soc) {
        (true, Some(soc)) => {
            let w = num(&full["total_pbattery"]).ok_or_else(|| missing("invert_full.total_pbattery"))?;
            // The direction comes from bms_status when it says so ("discharge" contains "charge",
            // so test it first); otherwise from the sign SEMS sends.
            let bms = inv["d"]["bms_status"].as_str().unwrap_or("").to_ascii_lowercase();
            let watts = if bms.contains("discharge") {
                w.abs()
            } else if bms.contains("charge") {
                -w.abs()
            } else {
                w
            };
            let capacity_kwh = num(&info["battery_capacity"]).filter(|c| *c > 0.0);
            Some(Battery { soc, watts, capacity_kwh })
        }
        _ => None,
    };

    Ok(Reading {
        time,
        grid: grid_signal(vac1, grid_status.as_deref()),
        vac1,
        grid_status,
        battery,
        utc_offset_h: num(&info["time_span"]).map(|s| -s),
    })
}

/// `hems/power/statisticsAndPreV2` reply → today's `ChartSeries` for `day`.
/// Joins soc and pBat by `tp`, drops null and `isCE` points and anything not on `day`.
pub fn parse_chart(v: &Value, day: i64) -> Result<ChartSeries, String> {
    check_plus_code(v)?;
    let list = v["data"]["dataList"].as_array().ok_or_else(|| missing("data.dataList"))?;
    let series = |item: &str| -> Result<Vec<(u32, f64)>, String> {
        let s = list.iter().find(|s| s["item"] == item).ok_or_else(|| missing(&format!("dataList[item={item}]")))?;
        let pts = s["powerData"].as_array().ok_or_else(|| missing(&format!("{item}.powerData")))?;
        Ok(pts
            .iter()
            .filter(|p| p["isCE"] != Value::Bool(true))
            .filter_map(|p| {
                let t = LocalTs::parse(p["tp"].as_str()?)?;
                (t.day() == day).then_some((t.minute_of_day(), num(&p["power"])?))
            })
            .collect())
    };
    let soc = series("soc")?;
    let kw: std::collections::HashMap<u32, f64> = series("pBat")?.into_iter().collect();
    let mut points: Vec<_> = soc.into_iter().filter_map(|(m, s)| Some((m, s, *kw.get(&m)?))).collect();
    points.sort_by_key(|p| p.0);
    points.dedup_by_key(|p| p.0);
    Ok(ChartSeries { day, points })
}

fn check_classic_code(v: &Value) -> Result<(), String> {
    match &v["code"] {
        Value::Number(n) if n.as_i64() == Some(0) => Ok(()),
        Value::String(s) if s == "0" => Ok(()),
        Value::Null => Err(missing("code")),
        c => Err(format!("code {c}: {}", v["msg"].as_str().unwrap_or(""))),
    }
}

fn check_plus_code(v: &Value) -> Result<(), String> {
    match v["code"].as_str() {
        Some("00000") => Ok(()),
        Some(c) => Err(format!("code {c}: {}", v["description"].as_str().unwrap_or(""))),
        None => Err(missing("code")),
    }
}

/// A login that did not succeed. Only SEMS's bad-credential code (100005, "Email or password
/// error.", classic and SEMS+ alike, checked 2026-10-18) is `WrongLogin`; any other code or a
/// missing token is a passing failure (`Network`), so an outage hiccup never signs the user out.
fn login_refused(v: &Value, msg_key: &str) -> SemsError {
    let msg = v[msg_key].as_str().unwrap_or("no token").to_owned();
    match &v["code"] {
        Value::Number(n) if n.as_i64() == Some(100005) => SemsError::WrongLogin(msg),
        Value::String(s) if s == "100005" => SemsError::WrongLogin(msg),
        c => SemsError::Network(format!("login failed, code {c}: {msg}")),
    }
}

/// Classic codes that mean "log in again" (seen in open-source SEMS clients).
fn classic_expired(v: &Value) -> bool {
    let code = match &v["code"] {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return false,
    };
    matches!(code.as_str(), "100001" | "100002")
        || v["msg"].as_str().is_some_and(|m| m.to_ascii_lowercase().contains("authoriz"))
}

// ---------- the SEMS+ signature ----------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
/// SEMS+ sends the password as base64 of its MD5 hex string.
fn plus_pwd(pw: &str) -> String {
    B64.encode(hex(&Md5::digest(pw.as_bytes())))
}
/// `base64(sha256hex("{ts}@{uid}@{token}") + "@" + ts)`, fresh per request.
fn x_signature(ts_ms: u128, uid: &str, token: &str) -> String {
    let h = hex(&Sha256::digest(format!("{ts_ms}@{uid}@{token}").as_bytes()));
    B64.encode(format!("{h}@{ts_ms}"))
}
fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

// ---------- the client ----------

#[derive(Clone)]
pub struct Credentials {
    pub account: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }") // never print the secret
    }
}

struct Session {
    /// The login reply's `data`, JSON-encoded: sent as the `Token` header.
    token: String,
    api: String,
    station: String,
    /// SEMS+ only: uid and token for the signature.
    uid: String,
    raw_token: String,
}

pub struct Client {
    http: reqwest::Client,
    creds: Option<Credentials>,
    classic: Option<Session>,
    plus: Option<Session>,
}

impl Client {
    pub fn new(creds: Option<Credentials>) -> Client {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client"); // only fails if the TLS backend can't start
        Client { http, creds, classic: None, plus: None }
    }

    pub fn signed_in(&self) -> bool {
        self.creds.is_some()
    }

    /// The signed-in email, to prefill the sign-in box (from memory, not the Keychain).
    pub fn account(&self) -> Option<String> {
        self.creds.as_ref().map(|c| c.account.clone())
    }

    /// Checks the credentials with SEMS and keeps them for later polls. The caller saves them
    /// to the Keychain only after this returns Ok.
    pub async fn sign_in(&mut self, creds: Credentials) -> Result<(), SemsError> {
        let s = self.classic_login(&creds).await?;
        self.classic = Some(s);
        self.plus = None;
        self.creds = Some(creds);
        Ok(())
    }

    pub fn sign_out(&mut self) {
        (self.creds, self.classic, self.plus) = (None, None, None);
    }

    /// The live reading; logs in when needed and once more on an expired token.
    pub async fn reading(&mut self) -> Result<Reading, SemsError> {
        for attempt in 0..2 {
            if self.classic.is_none() {
                let creds = self.creds.clone().ok_or(SemsError::SignedOut)?;
                self.classic = Some(self.classic_login(&creds).await?);
            }
            let s = self.classic.as_ref().expect("set above");
            let url = format!("{}v1/PowerStation/GetMonitorDetailByPowerstationId", s.api);
            let (v, body) = self.post_classic(&url, &s.token, json!({ "powerStationId": s.station })).await?;
            if classic_expired(&v) && attempt == 0 {
                self.classic = None;
                continue;
            }
            return parse_reading(&v).map_err(|reason| SemsError::Parse { reason, reply: body });
        }
        Err(SemsError::Expired)
    }

    /// Today's chart series from the SEMS+ gateway, for the station-local `today`.
    pub async fn chart(&mut self, today: LocalTs, utc_offset_h: Option<f64>) -> Result<ChartSeries, SemsError> {
        let date = today.date();
        // timeZone is required (an empty list without it); SEMS+ labels `tp` station-local anyway.
        let tz = utc_offset_h.unwrap_or(0.0).round() as i64;
        for attempt in 0..2 {
            if self.plus.is_none() {
                let creds = self.creds.clone().ok_or(SemsError::SignedOut)?;
                self.plus = Some(self.plus_login(&creds).await?);
            }
            let s = self.plus.as_ref().expect("set above");
            let body = json!({
                "stationId": s.station, "items": ["soc", "pBat"], "timeScale": 2, "timeZone": tz,
                "startTime": format!("{date} 00:00:00"), "endTime": format!("{date} 23:59:59"),
            });
            let url = format!("{}/sems-plant/api/v1/hems/power/statisticsAndPreV2", s.api);
            match self.post_plus(&url, s, body).await {
                Err(SemsError::Expired) if attempt == 0 => {
                    self.plus = None;
                    continue;
                }
                Err(e) => return Err(e),
                Ok((v, raw)) => {
                    return parse_chart(&v, today.day()).map_err(|reason| SemsError::Parse { reason, reply: raw })
                }
            }
        }
        Err(SemsError::Expired)
    }

    async fn classic_login(&self, c: &Credentials) -> Result<Session, SemsError> {
        let body = json!({ "account": c.account, "pwd": c.password, "agreement_agreement": 1, "is_local": false });
        let (v, raw) = self.post_classic(CLASSIC_LOGIN, CLASSIC_TOKEN0, body).await?;
        let data = &v["data"];
        // Without a token the login did not succeed, whatever the message says.
        if check_classic_code(&v).is_err() || data["token"].as_str().is_none_or(str::is_empty) {
            return Err(login_refused(&v, "msg"));
        }
        let parse_err = |reason: String| SemsError::Parse { reason, reply: raw.clone() };
        let api = v["api"].as_str().ok_or_else(|| parse_err(missing("api")))?;
        let api = if api.ends_with('/') { api.to_owned() } else { format!("{api}/") };
        let token = data.to_string();
        let url = format!("{api}v2/HistoryData/QueryPowerStationByHistory");
        let (sv, sraw) = self.post_classic(&url, &token, json!({})).await?;
        let station = sv["data"]["list"][0]["id"]
            .as_str()
            .ok_or_else(|| SemsError::Parse { reason: missing("data.list[0].id"), reply: sraw })?
            .to_owned();
        Ok(Session { token, api, station, uid: String::new(), raw_token: String::new() })
    }

    async fn plus_login(&self, c: &Credentials) -> Result<Session, SemsError> {
        let t0 = json!({"uid":"","token":"","timestamp":0,"client":"semsPlusWeb","version":"","language":"en"});
        let body = json!({ "account": c.account, "pwd": plus_pwd(&c.password), "agreement": 1, "isChinese": false, "isLocal": false });
        let anon = Session { token: t0.to_string(), api: String::new(), station: String::new(), uid: String::new(), raw_token: String::new() };
        let (v, raw) = match self.post_plus(PLUS_LOGIN, &anon, body).await {
            Err(SemsError::Parse { reason, reply }) if reason.starts_with("code ") => {
                return Err(login_refused(&serde_json::from_str(&reply).unwrap_or_default(), "description"))
            }
            r => r?,
        };
        let parse_err = |reason: String| SemsError::Parse { reason, reply: raw.clone() };
        let data = &v["data"];
        let uid = data["uid"].as_str().ok_or_else(|| parse_err(missing("data.uid")))?.to_owned();
        let raw_token = data["token"].as_str().ok_or_else(|| parse_err(missing("data.token")))?.to_owned();
        let api = data["api"].as_str().ok_or_else(|| parse_err(missing("data.api")))?.trim_end_matches('/').to_owned();
        let mut s = Session { token: data.to_string(), api, station: String::new(), uid, raw_token };
        let (sv, sraw) = self.post_plus(&format!("{}/sems-plant/api/stations/simple-query", s.api), &s, json!({})).await?;
        s.station = sv["data"]["dataList"][0]["id"]
            .as_str()
            .ok_or_else(|| SemsError::Parse { reason: missing("data.dataList[0].id"), reply: sraw })?
            .to_owned();
        Ok(s)
    }

    async fn post_classic(&self, url: &str, token: &str, body: Value) -> Result<(Value, String), SemsError> {
        let req = self.http.post(url).header("Token", token).json(&body);
        let (v, raw) = send(req).await?;
        Ok((v, raw))
    }

    /// A signed SEMS+ call. Maps 401/C0602 to `Expired` and any other non-"00000" code to `Parse`.
    async fn post_plus(&self, url: &str, s: &Session, body: Value) -> Result<(Value, String), SemsError> {
        let req = self
            .http
            .post(url)
            .header("Token", &s.token)
            .header("X-Signature", x_signature(now_ms(), &s.uid, &s.raw_token))
            .header("Origin", PLUS_ORIGIN)
            .header("Referer", PLUS_ORIGIN)
            .header("User-Agent", UA)
            .header("Accept", "application/json, text/plain, */*")
            .json(&body);
        let (v, raw) = send(req).await?;
        match v["code"].as_str() {
            Some("C0602") => Err(SemsError::Expired),
            Some("GY0429") => Err(SemsError::Network("rate limited".into())),
            _ => check_plus_code(&v).map(|_| (v, raw.clone())).map_err(|reason| SemsError::Parse { reason, reply: raw }),
        }
    }
}

async fn send(req: reqwest::RequestBuilder) -> Result<(Value, String), SemsError> {
    let resp = req.send().await.map_err(|e| SemsError::Network(e.without_url().to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(SemsError::Expired);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(SemsError::Network(format!("HTTP {status}")));
    }
    let raw = resp.text().await.map_err(|e| SemsError::Network(e.without_url().to_string()))?;
    let v = serde_json::from_str(&raw)
        .map_err(|e| SemsError::Parse { reason: format!("not JSON (HTTP {status}): {e}"), reply: raw.clone() })?;
    Ok((v, raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn samples_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }
    fn sample(name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(samples_dir().join(name)).unwrap()).unwrap()
    }

    #[test]
    fn grid_rule_one_reading() {
        assert_eq!(grid_signal(0.1, Some("generating_off_grid")), GridSignal::Off);
        assert_eq!(grid_signal(231.0, Some("generating_on_grid")), GridSignal::On);
        assert_eq!(grid_signal(49.9, Some("generating_on_grid")), GridSignal::Disagree);
        assert_eq!(grid_signal(230.0, Some("generating_off_grid")), GridSignal::Disagree);
        assert_eq!(grid_signal(50.0, None), GridSignal::On); // no status field: voltage alone
        assert_eq!(grid_signal(3.0, None), GridSignal::Off);
    }

    #[test]
    fn every_sample_parses() {
        let mut n = 0;
        for e in std::fs::read_dir(samples_dir()).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            if name.starts_with("monitor-detail-") {
                parse_reading(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
            } else if name.starts_with("sems-plus-power-history-") {
                let day = LocalTs::parse(v["data"]["dataList"][0]["powerData"][0]["tp"].as_str().unwrap()).unwrap().day();
                let c = parse_chart(&v, day).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(!c.points.is_empty(), "{name}: no points");
            } else {
                panic!("{name}: unknown sample kind; name it monitor-detail-* or sems-plus-power-history-* and add a check");
            }
            n += 1;
        }
        assert!(n >= 2);
    }

    #[test]
    fn grid_off_sample_reading() {
        let r = parse_reading(&sample("monitor-detail-grid-off.json")).unwrap();
        assert_eq!(r.time, LocalTs::parse("10/18/2026 08:21:38").unwrap());
        assert_eq!(r.grid, GridSignal::Off);
        assert_eq!(r.vac1, 0.1);
        assert_eq!(r.grid_status.as_deref(), Some("generating_off_grid"));
        let b = r.battery.unwrap();
        assert_eq!((b.soc, b.watts, b.capacity_kwh), (86.0, 998.28, Some(5.1)));
        assert_eq!(r.utc_offset_h, Some(5.0));
    }

    #[test]
    fn day_series_sample() {
        let day = LocalTs::parse("2026-10-18 00:00:00").unwrap().day();
        let c = parse_chart(&sample("sems-plus-power-history-day.json"), day).unwrap();
        assert_eq!(c.points.first(), Some(&(0, 93.0, 0.87412)));
        assert_eq!(c.points.last().unwrap().0, 9 * 60 + 6); // the 23:59 isCE sentinel is gone
        assert!(c.points.windows(2).all(|w| w[0].0 < w[1].0));
        // another day's points are dropped
        assert!(parse_chart(&sample("sems-plus-power-history-day.json"), day + 1).unwrap().points.is_empty());
    }

    #[test]
    fn numeric_strings_and_missing_fields() {
        let mut v = sample("monitor-detail-grid-off.json");
        v["data"]["inverter"][0]["invert_full"]["vac1"] = json!("231.4");
        v["data"]["inverter"][0]["invert_full"]["soc"] = json!("62");
        v["data"]["inverter"][0]["invert_full"]["grid_conn_status"] = json!("generating_on_grid");
        v["data"]["inverter"][0]["d"]["bms_status"] = json!("charge");
        let r = parse_reading(&v).unwrap();
        assert_eq!((r.grid, r.vac1, r.battery.as_ref().unwrap().soc), (GridSignal::On, 231.4, 62.0));
        assert!(r.battery.unwrap().watts < 0.0, "charging is negative");

        v["data"]["inverter"][0]["invert_full"].as_object_mut().unwrap().remove("vac1");
        assert_eq!(parse_reading(&v).unwrap_err(), "field `invert_full.vac1` missing");
        assert_eq!(parse_reading(&json!({"code": 0, "data": {}})).unwrap_err(), "field `info.time` missing");
        assert!(parse_reading(&json!({"code": 100002, "msg": "x"})).unwrap_err().starts_with("code 100002"));
    }

    #[test]
    fn no_battery_station() {
        let mut v = sample("monitor-detail-grid-off.json");
        v["data"]["info"]["is_stored"] = json!(false);
        assert_eq!(parse_reading(&v).unwrap().battery, None);
        let mut v = sample("monitor-detail-grid-off.json");
        v["data"]["inverter"][0]["invert_full"]["soc"] = Value::Null;
        assert_eq!(parse_reading(&v).unwrap().battery, None);
    }

    #[test]
    fn signature_matches_node() {
        // expected values computed with node's crypto (same scheme as the SEMS+ web client)
        assert_eq!(plus_pwd("hunter2"), "MmFiOTYzOTBjN2RiZTM0MzlkZTc0ZDBjOWIwYjE3Njc=");
        assert_eq!(
            x_signature(1_700_000_000_000, "", ""),
            "NjcyMjMxZWJhYWUwMjZiZDFhYWNlZjE4MTdkNzdhOGJmYzc2YWM4NzBhNDFiYTQ3MzA1MDYzNDYyODRmZDI0N0AxNzAwMDAwMDAwMDAw"
        );
    }

    #[test]
    fn expired_codes() {
        assert_eq!(login_refused(&json!({"code": 100005, "msg": "Email or password error."}), "msg"), SemsError::WrongLogin("Email or password error.".into()));
        assert_eq!(login_refused(&json!({"code": "100005", "description": "x"}), "description"), SemsError::WrongLogin("x".into()));
        assert!(matches!(login_refused(&json!({"code": 1, "msg": "system error"}), "msg"), SemsError::Network(_)));
        assert!(matches!(login_refused(&json!({"code": 0, "data": {}}), "msg"), SemsError::Network(_)), "no token");
        assert!(classic_expired(&json!({"code": 100002, "msg": "x"})));
        assert!(classic_expired(&json!({"code": "100001"})));
        assert!(classic_expired(&json!({"code": 1, "msg": "The authorization has expired"})));
        assert!(!classic_expired(&json!({"code": 0})));
    }

    /// One real round trip with the owner's dev login from ../.env. Prints nothing secret.
    /// Run: cargo test --manifest-path src-tauri/Cargo.toml live_smoke -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_smoke() {
        let env = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.env")).unwrap();
        let get = |k: &str| {
            env.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned())
                .unwrap()
        };
        let mut c = Client::new(None);
        c.sign_in(Credentials { account: get("SEMS_ACCOUNT"), password: get("SEMS_PASSWORD") }).await.unwrap_or_else(|e| panic!("{e}")); // Display never shows the reply body
        let r = c.reading().await.unwrap_or_else(|e| panic!("{e}"));
        println!("reading at {} {}: grid {:?} (vac1 {}, status {:?}), battery {:?}", r.time.date(), r.time.hhmm(), r.grid, r.vac1, r.grid_status, r.battery);
        let ch = c.chart(r.time, r.utc_offset_h).await.unwrap_or_else(|e| panic!("{e}"));
        println!("chart: {} points, first {:?}, last {:?}", ch.points.len(), ch.points.first(), ch.points.last());
        let same = c.classic.as_ref().unwrap().station == c.plus.as_ref().unwrap().station;
        println!("classic and SEMS+ station ids equal: {same}");
        let leaked = std::fs::read_to_string(samples_dir().join("monitor-detail-grid-off.json")).unwrap().contains(&c.classic.as_ref().unwrap().station);
        println!("live station id present in the saved sample: {leaked}");

        // B-R1: a dump of the live reply keeps none of the personal values (never printed).
        let st = c.classic.as_ref().unwrap();
        let url = format!("{}v1/PowerStation/GetMonitorDetailByPowerstationId", st.api);
        let (v, _) = c.post_classic(&url, &st.token, json!({ "powerStationId": st.station })).await.unwrap();
        let inv = &v["data"]["inverter"][0];
        let mut secrets = vec![st.station.clone(), get("SEMS_ACCOUNT")];
        secrets.extend(["sn", "check_code"].iter().filter_map(|k| inv["invert_full"][*k].as_str().map(str::to_owned)));
        secrets.extend(inv["dict"]["left"].as_array().into_iter().flatten().filter(|x| ["serialNum", "laCheckcode"].contains(&x["key"].as_str().unwrap_or(""))).filter_map(|x| x["value"].as_str().map(str::to_owned)));
        secrets.retain(|x| x.len() >= 4);
        let out = crate::logdir::redact(&v).to_string().to_lowercase();
        let survived = secrets.iter().filter(|x| out.contains(&x.to_lowercase())).count();
        println!("redaction: {} personal values checked, {survived} survived", secrets.len());
        assert_eq!(survived, 0);

        // B-R6: the station list (read at every classic login) holds the address.
        let (sl, _) = c.post_classic(&format!("{}v2/HistoryData/QueryPowerStationByHistory", st.api), &st.token, json!({})).await.unwrap();
        let s0 = &sl["data"]["list"][0];
        let mut secrets: Vec<String> = ["id", "pw_name", "pw_address"].iter().filter_map(|k| s0[*k].as_str().map(str::to_owned)).collect();
        secrets.extend(v["data"]["info"]["address"].as_str().map(str::to_owned));
        secrets.retain(|x| x.len() >= 4);
        let out = crate::logdir::redact(&sl).to_string().to_lowercase();
        let survived = secrets.iter().filter(|x| out.contains(&x.to_lowercase())).count();
        println!("station list redaction: {} personal values checked, {survived} survived", secrets.len());
        assert_eq!(survived, 0);
    }
}
