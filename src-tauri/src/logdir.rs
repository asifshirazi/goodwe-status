//! The log folder (~/Library/Logs/dev.goodwestatus.menubar on macOS): a small text log and
//! redacted dumps of replies the app could not read, ready for a GitHub issue.

use crate::clock::LocalTs;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const LOG: &str = "goodwe-status.log";
const LOG_MAX_BYTES: u64 = 1 << 20;
const KEEP_DUMPS: usize = 10;
/// The same failure again is logged at most this often, with how many were held back (T-026).
const REPEAT_EVERY_S: u64 = 30 * 60;
/// ...and its reply dumped at most this often, so a broken API can't rotate the first dump away.
const DUMP_EVERY_S: u64 = 6 * 3600;

/// Keys whose values are personal (ids, serials, names, places, secrets). Matched lower-case.
const PERSONAL: &[&str] = &[
    "token", "pwd", "password", "account", "email", "phone", "address", "latitude", "longitude", "lat", "lng",
    "check_code", "org_code", "org_name", "stationname", "sns", "parallelsnlist", "parallelbat_sn_list",
];

fn personal(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    PERSONAL.contains(&k.as_str())
        || k.ends_with("id") // id, uid, pw_id, powerstation_id, relation_id, traceId, ...
        || k.ends_with("sn") || k.contains("_sn") // sn, batterysnmain, dataloggersn, target_sn_six, ...
        || k.contains("snmain")
        || k.contains("name") // name, owner_name, target_name, equipment_name, ...
        || k.starts_with("owner")
        || k.contains("serial") // serialNum, ...
        || k.contains("checkcode") // laCheckcode, ...
        || k.contains("address") // address, pw_address (station list), ...
        || k.contains("token") // access_token, refresh_token (Tuya), ...
        || k.ends_with("_key") || k.contains("secret") // local_key (Tuya), ...
        || ["ip", "lon"].contains(&k.as_str()) // Tuya device list
}

const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";

fn is_uuid(b: &[u8]) -> bool {
    b.len() == 36 && b.iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

/// Any UUID-shaped run (station ids also hide inside JSON strings such as `components.para`).
fn scrub_uuids(s: &str) -> String {
    let (b, mut out, mut i) = (s.as_bytes(), String::with_capacity(s.len()), 0);
    while i < b.len() {
        if b.len() - i >= 36 && is_uuid(&b[i..i + 36]) {
            out.push_str(ZERO_UUID);
            i += 36;
        } else {
            let c = s[i..].chars().next().expect("i is on a char boundary");
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Byte length of the quoted string at the start of `s` (with its `\` escapes; to the end
/// of the text when it is cut off).
fn quoted_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut end = 1;
    while end < b.len() && b[end] != b'"' {
        end += if b[end] == b'\\' { 2 } else { 1 };
    }
    let mut end = (end + 1).min(s.len());
    while !s.is_char_boundary(end) {
        end += 1; // a stray `\` before a multi-byte char
    }
    end
}

/// Byte length of the unquoted JSON value at the start of `s`: an object or array up to its
/// matching close bracket (quoted strings skipped, so a `,` or `]` inside one doesn't count;
/// to the end of the text when it is cut off), else a scalar up to the next `,` `}` `]`.
fn value_len(s: &str) -> usize {
    if !s.starts_with(['{', '[']) {
        return s.find([',', '}', ']']).unwrap_or(s.len());
    }
    let (mut depth, mut i) = (0usize, 0);
    while i < s.len() {
        match s.as_bytes()[i] {
            b'"' => {
                i += quoted_len(&s[i..]);
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// A reply that isn't JSON (cut off, an error page), kept as text: UUIDs zeroed, email-shaped
/// words and the value after any personal `"key":` replaced, as `redact` does for JSON.
fn scrub_text(raw: &str) -> String {
    let s = scrub_uuids(raw);
    let word = |c: char| c.is_alphanumeric() || "._%+-@".contains(c);
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    let mut hide_next = false; // the last quoted string was a personal key followed by ':'
    while let Some(c) = rest.chars().next() {
        if c == '"' {
            let (q, after) = rest.split_at(quoted_len(rest));
            let inner = q.trim_matches('"');
            if hide_next {
                out.push_str("\"redacted\"");
                hide_next = false;
            } else {
                out.push_str(&if inner.contains('@') && inner.contains('.') { "\"redacted\"".to_owned() } else { q.to_owned() });
                hide_next = personal(inner) && after.trim_start().starts_with(':');
            }
            rest = after;
        } else if hide_next && c != ':' && !c.is_whitespace() {
            let n = value_len(rest);
            out.push_str("\"redacted\"");
            rest = &rest[n..];
            hide_next = false;
        } else if word(c) && c != '.' {
            let n = rest.find(|x: char| !word(x)).unwrap_or(rest.len());
            let w = &rest[..n];
            out.push_str(if w.contains('@') && w.contains('.') { "redacted" } else { w });
            rest = &rest[n..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Replaces personal values with "redacted", recursively: by key, by the sibling `key` of a
/// label/value pair (`dict.left[]`: `{"key": "serialNum", "value": …}`), any string that looks
/// like an email; and any UUID inside a string becomes the zero UUID.
pub fn redact(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let label_personal = m.get("key").and_then(Value::as_str).is_some_and(personal);
            Value::Object(
                m.iter()
                    .map(|(k, x)| {
                        let hide = personal(k) || (label_personal && k == "value");
                        (k.clone(), if hide && !x.is_null() { json!("redacted") } else { redact(x) })
                    })
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        Value::String(s) if s.contains('@') && s.contains('.') => json!("redacted"),
        Value::String(s) => json!(scrub_uuids(s)),
        x => x.clone(),
    }
}

fn utc_stamp() -> (u64, String) {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let t = LocalTs(s as i64);
    (s, format!("{}T{}:{:02}Z", t.date(), t.hhmm(), s % 60))
}

/// Appends one line to the log, rolling it over at 1 MB. Never put secrets in `msg`.
pub fn line(dir: &Path, msg: &str) {
    let _ = fs::create_dir_all(dir);
    let p = dir.join(LOG);
    if fs::metadata(&p).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = fs::rename(&p, dir.join(format!("{LOG}.1")));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{} {msg}", utc_stamp().1);
    }
}

/// One failed call for the log (T-026). `call`: "sems/reading", "tuya/devices"…; `kind`: what
/// went wrong, without dates or values (repeats of one call + kind are rate-limited); `line`:
/// the whole entry; `reply`: the raw body, dumped redacted when the line is written.
#[derive(Debug, Clone, PartialEq)]
pub struct Fail {
    pub call: String,
    pub kind: String,
    pub line: String,
    pub reply: Option<String>,
}

#[derive(Debug)]
struct Seen {
    logged: u64,
    dumped: Option<u64>,
    held: u32,
    /// Failures since the call last worked.
    since_ok: u32,
    /// A line for this key reached the log since the last `OK` line: the next success writes one.
    owe_ok: bool,
}

/// Repeats per `call|kind` key, in memory (a restart logs the first one again). A key outlives a
/// success, so a call that flaps (fail, ok, fail…) keeps its 30-minute window and repeat count
/// instead of writing a FAIL and an OK line every other poll (T-026 B-Y2).
#[derive(Debug, Default)]
struct Limiter(BTreeMap<String, Seen>);

impl Limiter {
    /// `Some(n)`: write this one (n = repeats held back since the last line); `None`: hold it back.
    fn hit(&mut self, key: &str, now: u64) -> Option<u32> {
        let Some(s) = self.0.get_mut(key) else {
            self.0.insert(key.to_owned(), Seen { logged: now, dumped: None, held: 0, since_ok: 1, owe_ok: true });
            return Some(0);
        };
        s.since_ok += 1;
        if now >= s.logged + REPEAT_EVERY_S {
            (s.logged, s.owe_ok) = (now, true);
            return Some(std::mem::take(&mut s.held));
        }
        s.held += 1;
        None
    }

    fn dump_due(&mut self, key: &str, now: u64) -> bool {
        let Some(s) = self.0.get_mut(key) else { return false };
        let due = s.dumped.is_none_or(|d| now >= d + DUMP_EVERY_S);
        if due {
            s.dumped = Some(now);
        }
        due
    }

    /// `call` worked: its failures since it last worked, when one of them reached the log since the
    /// last `OK` line (else 0: nothing to say, the held-back ones show in the next FAIL line's count).
    fn ok(&mut self, call: &str) -> u32 {
        let p = format!("{call}|");
        let (mut n, mut owed) = (0, false);
        for s in self.0.iter_mut().filter(|(k, _)| k.starts_with(&p)).map(|(_, s)| s) {
            n += std::mem::take(&mut s.since_ok);
            owed |= std::mem::take(&mut s.owe_ok);
        }
        if owed {
            n
        } else {
            0
        }
    }
}

/// One limiter per log folder (tests each use their own).
static LIMITS: Mutex<BTreeMap<PathBuf, Limiter>> = Mutex::new(BTreeMap::new());

fn limits<T>(dir: &Path, f: impl FnOnce(&mut Limiter) -> T) -> T {
    let mut all = LIMITS.lock().unwrap_or_else(|e| e.into_inner());
    f(all.entry(dir.to_owned()).or_default())
}

/// Logs a failure, unless the same one was logged under 30 min ago (then it is counted and the
/// next line says how many were held back). Its reply, if any, is dumped at most once per 6 h.
/// Emails and UUIDs in the line are scrubbed as a last guard; never put ids or secrets in it.
pub fn fail(dir: &Path, f: &Fail) {
    fail_at(dir, f, utc_stamp().0)
}

fn fail_at(dir: &Path, f: &Fail, now: u64) {
    let key = format!("{}|{}", f.call, f.kind);
    let Some((held, dump)) = limits(dir, |l| l.hit(&key, now).map(|h| (h, f.reply.is_some() && l.dump_due(&key, now)))) else { return };
    let mut msg = scrub_text(&f.line);
    if held > 0 {
        msg += &format!(" (repeated {held} times since the last line)");
    }
    if let (true, Some(r)) = (dump, &f.reply) {
        msg += &format!(" · saved {}", dump_bad_reply(dir, &f.kind, r));
    }
    line(dir, &msg);
}

/// `call` worked: one `OK` line when it had failed since it last worked.
pub fn ok(dir: &Path, call: &str) {
    let n = limits(dir, |l| l.ok(call));
    if n > 0 {
        line(dir, &format!("OK {call} · working again after {n} failed {}", if n == 1 { "call" } else { "calls" }));
    }
}

/// Writes `bad-reply-<time>-<nn>.json` with the reason and the redacted reply, keeps the newest 10,
/// and returns its name. A reply that isn't JSON is kept as text, cut to 4 KB.
pub fn dump_bad_reply(dir: &Path, reason: &str, raw: &str) -> String {
    let _ = fs::create_dir_all(dir);
    let reply = match serde_json::from_str::<Value>(raw) {
        Ok(v) => redact(&v),
        Err(_) => json!(scrub_text(&raw.chars().take(4096).collect::<String>())),
    };
    let (secs, stamp) = utc_stamp();
    // Several bad replies can come in one second (T-012 goes on after one): never overwrite.
    let name = (0..)
        .map(|n| format!("bad-reply-{secs}-{n:02}.json"))
        .find(|n| !dir.join(n).exists())
        .unwrap_or_default();
    let body = json!({ "time": stamp, "reason": reason, "reply": reply });
    let _ = fs::write(dir.join(&name), serde_json::to_string_pretty(&body).unwrap_or_default());

    let mut dumps: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("bad-reply-"))
        .collect();
    dumps.sort(); // same digit count for centuries, so this is oldest first
    for old in dumps.iter().rev().skip(KEEP_DUMPS) {
        let _ = fs::remove_file(dir.join(old));
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_personal_fields_in_the_sample() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitor-detail-grid-off.json");
        let v: Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
        let r = redact(&v);
        let info = &r["data"]["info"];
        for k in ["powerstation_id", "stationname", "address", "org_code"] {
            assert_eq!(info[k], "redacted", "{k}");
        }
        let full = &r["data"]["inverter"][0]["invert_full"];
        assert_eq!(full["sn"], "redacted");
        // the values the parser needs survive
        assert_eq!(full["vac1"], v["data"]["inverter"][0]["invert_full"]["vac1"]);
        assert_eq!(info["time"], v["data"]["info"]["time"]);
        assert_eq!(redact(&json!({"msg": "x", "who": "a@b.co", "uid": 7})), json!({"msg": "x", "who": "redacted", "uid": "redacted"}));
    }

    #[test]
    fn redacts_label_value_lists_and_uuids_in_strings() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitor-detail-grid-off.json");
        let mut v: Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
        // real-looking values where a live reply has them (made up here)
        let (sn, code, station) = ("95048ESU224W0123", "804512", "3f2c9a1e-7b4d-4c2a-9e1f-5a6b7c8d9e0f");
        v["data"]["inverter"][0]["dict"]["left"][1]["value"] = json!(sn);
        v["data"]["inverter"][0]["dict"]["left"][2]["value"] = json!(code);
        v["components"]["para"] = json!(format!(r#"{{"powerStationId":"{}","x":1}}"#, station.to_uppercase()));
        v["data"]["inverter"][0]["invert_full"]["errors"][0]["error_id"] = json!(31337);
        let out = redact(&v).to_string();
        for secret in [sn, code, station, &station.to_uppercase(), "31337"] {
            assert!(!out.contains(secret), "{secret} survived");
        }
        let r = redact(&v);
        assert_eq!(r["data"]["inverter"][0]["dict"]["left"][1]["key"], "serialNum", "labels stay");
        assert_eq!(r["data"]["inverter"][0]["dict"]["left"][3], v["data"]["inverter"][0]["dict"]["left"][3], "capacity stays");
        assert!(r["components"]["para"].as_str().unwrap().contains(ZERO_UUID));
        assert_eq!(scrub_uuids("é 3F2C9A1E-7B4D-4C2A-9E1F-5A6B7C8D9E0F!"), format!("é {ZERO_UUID}!"));
        assert_eq!(scrub_uuids("not-a-uuid"), "not-a-uuid");
    }

    #[test]
    fn redacts_the_station_list_and_cut_off_text() {
        // shaped like v2/HistoryData/QueryPowerStationByHistory (read at every classic login); made-up values
        let addr = "12 Example Road, Block 7, Sample City";
        let station = "3f2c9a1e-7b4d-4c2a-9e1f-5a6b7c8d9e0f";
        let reply = json!({"code": 0, "data": {"list": [{"id": station, "pw_name": "My Home", "pw_address": addr, "capacity": 5.0}]}});
        let out = redact(&reply).to_string();
        for secret in [addr, station, "My Home"] {
            assert!(!out.contains(secret), "{secret} survived");
        }
        assert!(out.contains("5.0"), "capacity stays");

        // the same reply cut off mid-way (not JSON), plus an email and a numeric id
        let text = format!(r#"{{"code":0,"data":{{"list":[{{"id":"{station}","pw_address": "{addr}","uid": 4711,"owner_email":"a.b@example.com","capacity":5.0,"note":"mail x.y@example.org now","pw_na"#);
        let out = scrub_text(&text);
        for secret in [addr, station, "4711", "a.b@example.com", "x.y@example.org"] {
            assert!(!out.contains(secret), "{secret} survived in {out}");
        }
        assert!(out.contains(r#""capacity":5.0"#) && out.contains(r#""code":0"#), "{out}");
        assert_eq!(scrub_text("<html>502 Bad Gateway</html>"), "<html>502 Bad Gateway</html>");

        // B-O1, the reviewer's inputs: an array and an object under a personal key, with a comma
        // inside a quoted string; the whole value goes, and the keys after it are still read.
        let out = scrub_text(r#"{"sns":["SNAAAA1111","SNBBBB2222"],"capacity":5.0,"x"#);
        assert_eq!(out, r#"{"sns":"redacted","capacity":5.0,"x"#);
        let out = scrub_text(r#"{"owner":{"street":"12 Road, Block 7","phone":"5550001"},"uid":4711,"pw_address":"9 Lane","capacity":5.0,"pw"#);
        for secret in ["12 Road", "Block 7", "5550001", "4711", "9 Lane"] {
            assert!(!out.contains(secret), "{secret} survived in {out}");
        }
        assert!(out.contains(r#""capacity":5.0"#), "{out}");
        // cut off inside the personal value: everything after the key goes
        assert_eq!(scrub_text(r#"{"parallelSnList":[{"sn":"SN1","n":"a, ] b"#), r#"{"parallelSnList":"redacted""#);
        assert_eq!(scrub_text("\"a\\é\" ok"), "\"a\\é\" ok", "no panic on a stray backslash");
    }

    #[test]
    fn dump_keeps_ten_and_logs() {
        let dir = std::env::temp_dir().join(format!("gw-logtest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for i in 0..12 {
            fs::write(dir.join(format!("bad-reply-{:010}.json", i)), "{}").unwrap();
        }
        let f = Fail { call: "sems/reading".into(), kind: "info.time".into(), line: "CHANGED sems/reading · field `info.time` missing".into(), reply: Some(r#"{"data":{"token":"secret","info":{}}}"#.into()) };
        fail(&dir, &f);
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.iter().filter(|n| n.starts_with("bad-reply-")).count(), 10);
        let all: String = names.iter().map(|n| fs::read_to_string(dir.join(n)).unwrap()).collect();
        assert!(!all.contains("secret"));
        let log = fs::read_to_string(dir.join(LOG)).unwrap();
        assert!(log.contains("field `info.time` missing · saved bad-reply-"), "{log}");
        fs::remove_dir_all(&dir).unwrap();
    }

    fn read_log(dir: &Path) -> Vec<String> {
        fs::read_to_string(dir.join(LOG)).unwrap_or_default().lines().map(|l| l.split_once(' ').unwrap().1.to_owned()).collect()
    }

    #[test]
    fn tuya_replies_are_redacted_in_the_dump() {
        // shaped like /v1.0/iot-01/associated-users/devices and /v1.0/token; made-up values
        let reply = json!({"devices": [{"id": "bf0123456789abcdefgh", "uuid": "ab12cd34ef56", "name": "Table LED", "product_name": "LED", "local_key": "k3y!value",
            "ip": "203.0.113.7", "lat": "24.86", "lon": "67.00", "owner_id": "1234567", "uid": "ay1600000000000aBcDe", "category": "dd", "online": true}],
            "access_token": "tok-access", "refresh_token": "tok-refresh", "expire_time": 7200});
        let out = redact(&reply).to_string();
        for secret in ["bf0123456789abcdefgh", "ab12cd34ef56", "Table LED", "k3y!value", "203.0.113.7", "24.86", "67.00", "1234567", "ay1600000000000aBcDe", "tok-access", "tok-refresh"] {
            assert!(!out.contains(secret), "{secret} survived in {out}");
        }
        assert!(out.contains(r#""category":"dd""#) && out.contains("7200"), "{out}");
        // and through logdir::fail: the dump file holds none of it either
        let dir = std::env::temp_dir().join(format!("gw-logtest-tuya-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fail(&dir, &Fail { call: "tuya/devices".into(), kind: "missing result.devices".into(), line: "CHANGED tuya/devices · x".into(), reply: Some(reply.to_string()) });
        let all: String = fs::read_dir(&dir).unwrap().map(|e| fs::read_to_string(e.unwrap().path()).unwrap()).collect();
        assert!(all.contains("saved bad-reply-") && !all.contains("k3y!value") && !all.contains("bf0123456789abcdefgh"), "{all}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn repeats_are_held_back_and_counted() {
        let dir = std::env::temp_dir().join(format!("gw-logtest-rate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let f = |kind: &str, reply: Option<&str>| Fail { call: "sems+/day".into(), kind: kind.into(), line: format!("FAIL sems+/day · {kind}"), reply: reply.map(str::to_owned) };
        let t0 = 1_000_000;
        fail_at(&dir, &f("network: timeout", None), t0);
        for i in 1..=5 {
            fail_at(&dir, &f("network: timeout", None), t0 + i * 60); // every 2 min: held back
        }
        fail_at(&dir, &f("code 500", None), t0 + 60); // another kind: its own line
        fail_at(&dir, &f("network: timeout", None), t0 + REPEAT_EVERY_S);
        assert_eq!(read_log(&dir), [
            "FAIL sems+/day · network: timeout",
            "FAIL sems+/day · code 500",
            "FAIL sems+/day · network: timeout (repeated 5 times since the last line)",
        ]);
        // recovery: one line for the call, counting every failure, then nothing more
        ok(&dir, "sems+/day");
        ok(&dir, "sems+/day");
        ok(&dir, "sems/reading");
        assert_eq!(read_log(&dir).last().unwrap(), "OK sems+/day · working again after 8 failed calls");
        assert_eq!(read_log(&dir).len(), 4);
        // B-Y2: a call that flaps (fail, ok, fail, ok every poll) stays quiet inside the 30 minutes:
        // no FAIL and no OK, and the held-back failures show in the next line's count
        for i in 1..=10 {
            fail_at(&dir, &f("network: timeout", None), t0 + REPEAT_EVERY_S + i * 120);
            ok(&dir, "sems+/day");
        }
        assert_eq!(read_log(&dir).len(), 4);
        fail_at(&dir, &f("network: timeout", None), t0 + 2 * REPEAT_EVERY_S);
        ok(&dir, "sems+/day");
        assert_eq!(read_log(&dir)[4..], ["FAIL sems+/day · network: timeout (repeated 10 times since the last line)", "OK sems+/day · working again after 1 failed call"]);

        // dumps: at most one per key per 6 h, even when the line itself is due
        let bad = Some(r#"{"code":"00000"}"#);
        fail_at(&dir, &f("missing pBat", bad), t0);
        fail_at(&dir, &f("missing pBat", bad), t0 + REPEAT_EVERY_S);
        fail_at(&dir, &f("missing pBat", bad), t0 + DUMP_EVERY_S);
        let dumps = fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("bad-reply-")).count();
        assert_eq!(dumps, 2);
        let log = read_log(&dir);
        assert!(log[6].contains("· saved bad-reply-") && !log[7].contains("saved") && log[8].contains("· saved bad-reply-"), "{log:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failure_lines_never_carry_an_email_or_station_id() {
        let dir = std::env::temp_dir().join(format!("gw-logtest-redact-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let line = "FAIL sems/login · login refused, code 1: user a.b@example.com station 3f2c9a1e-7b4d-4c2a-9e1f-5a6b7c8d9e0f";
        fail(&dir, &Fail { call: "sems/login".into(), kind: "code 1".into(), line: line.into(), reply: None });
        let log = fs::read_to_string(dir.join(LOG)).unwrap();
        for secret in ["a.b@example.com", "3f2c9a1e"] {
            assert!(!log.contains(secret), "{log}");
        }
        assert!(log.contains("FAIL sems/login · login refused, code 1: user redacted station 00000000-"), "{log}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dumps_in_one_second_get_their_own_files() {
        let dir = std::env::temp_dir().join(format!("gw-logtest-sec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let returned: Vec<_> = (0..4).map(|i| dump_bad_reply(&dir, &format!("reason {i}"), &format!(r#"{{"n":{i}}}"#))).collect();
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("bad-reply-")).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        names.sort();
        // oldest first, so the keep-10 prune still drops the oldest
        let reasons: Vec<_> = names.iter().map(|n| serde_json::from_str::<Value>(&fs::read_to_string(dir.join(n)).unwrap()).unwrap()["reason"].as_str().unwrap().to_owned()).collect();
        assert_eq!(reasons, ["reason 0", "reason 1", "reason 2", "reason 3"]);
        assert_eq!(returned, names, "each call names its own file");
        fs::remove_dir_all(&dir).unwrap();
    }
}
