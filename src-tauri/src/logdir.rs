//! The log folder (~/Library/Logs/dev.goodwestatus.menubar on macOS): a small text log and
//! redacted dumps of replies the app could not read, ready for a GitHub issue.

use crate::clock::LocalTs;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const LOG: &str = "goodwe-status.log";
const LOG_MAX_BYTES: u64 = 1 << 20;
const KEEP_DUMPS: usize = 10;

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

/// Writes `bad-reply-<time>-<nn>.json` with the reason and the redacted reply, keeps the newest 10,
/// and logs the reason. A reply that isn't JSON is kept as text, cut to 4 KB.
pub fn dump_bad_reply(dir: &Path, reason: &str, raw: &str) {
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
    line(dir, &format!("unreadable reply: {reason} (saved {name})"));

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
        dump_bad_reply(&dir, "field `info.time` missing", r#"{"data":{"token":"secret","info":{}}}"#);
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.iter().filter(|n| n.starts_with("bad-reply-")).count(), 10);
        let all: String = names.iter().map(|n| fs::read_to_string(dir.join(n)).unwrap()).collect();
        assert!(!all.contains("secret"));
        assert!(fs::read_to_string(dir.join(LOG)).unwrap().contains("field `info.time` missing"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dumps_in_one_second_get_their_own_files() {
        let dir = std::env::temp_dir().join(format!("gw-logtest-sec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for i in 0..4 {
            dump_bad_reply(&dir, &format!("reason {i}"), &format!(r#"{{"n":{i}}}"#));
        }
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("bad-reply-")).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        names.sort();
        // oldest first, so the keep-10 prune still drops the oldest
        let reasons: Vec<_> = names.iter().map(|n| serde_json::from_str::<Value>(&fs::read_to_string(dir.join(n)).unwrap()).unwrap()["reason"].as_str().unwrap().to_owned()).collect();
        assert_eq!(reasons, ["reason 0", "reason 1", "reason 2", "reason 3"]);
        let log = fs::read_to_string(dir.join(LOG)).unwrap();
        assert!(names.iter().all(|n| log.contains(n.as_str())), "each log line names its own file");
        fs::remove_dir_all(&dir).unwrap();
    }
}
