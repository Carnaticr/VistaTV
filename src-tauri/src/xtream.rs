//! Minimal Xtream Codes API client.
//!
//! Signs into an IPTV provider portal (host + username + password) and returns
//! its live channels as `ParsedChannel`s, reusing the same downstream storage as
//! M3U playlists. Xtream JSON is famously inconsistent (numbers vs strings), so
//! fields are read leniently from `serde_json::Value`.

use std::collections::HashMap;
use std::io::Read;

use serde_json::Value;

use crate::playlist::ParsedChannel;

/// Many IPTV/Xtream panels reject unknown User-Agents with odd 5xx codes,
/// so we present a common player UA for all provider/playlist requests.
pub const UA: &str = "VLC/3.0.20 LibVLC/3.0.20";

/// A shared HTTP agent: sends our UA and lets us read error-status bodies.
pub fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(UA)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Ensure the host has a scheme and no trailing slash.
fn normalize_base(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else {
        format!("http://{h}")
    }
}

/// Percent-encode a credential for safe use in URLs.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Read a JSON value that may be encoded as either a string or a number.
fn val_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn get_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let status = resp.status();
    let mut body = String::new();
    resp.body_mut()
        .as_reader()
        .take(256 * 1024 * 1024)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    let snippet: String = body.trim().chars().take(180).collect();
    if !status.is_success() {
        return Err(format!(
            "provider returned HTTP {}{}",
            status.as_u16(),
            if snippet.is_empty() { String::new() } else { format!(" — {snippet}") }
        ));
    }
    serde_json::from_str(&body).map_err(|_| {
        format!(
            "provider did not return JSON (got: {})",
            if snippet.is_empty() { "empty response".into() } else { snippet }
        )
    })
}

/// Authenticate and fetch all live streams as channels.
pub fn fetch_live(host: &str, user: &str, pass: &str) -> Result<Vec<ParsedChannel>, String> {
    let base = normalize_base(host);
    let (u, p) = (enc(user), enc(pass));
    let agent = agent();

    // 1. Authenticate.
    let info = get_json(&agent, &format!("{base}/player_api.php?username={u}&password={p}"))?;
    let authed = info
        .get("user_info")
        .and_then(|ui| ui.get("auth"))
        .map(|a| val_str(a) == "1")
        .unwrap_or(false);
    if !authed {
        return Err("sign-in failed: check the host, username and password".into());
    }

    // 2. Category id -> name map.
    let cats = get_json(&agent, &format!(
        "{base}/player_api.php?username={u}&password={p}&action=get_live_categories"
    ))?;
    let mut cat_map: HashMap<String, String> = HashMap::new();
    if let Some(arr) = cats.as_array() {
        for c in arr {
            cat_map.insert(val_str(&c["category_id"]), val_str(&c["category_name"]));
        }
    }

    // 3. Live streams -> channels.
    let streams = get_json(&agent, &format!(
        "{base}/player_api.php?username={u}&password={p}&action=get_live_streams"
    ))?;
    let mut out = Vec::new();
    if let Some(arr) = streams.as_array() {
        out.reserve(arr.len());
        for s in arr {
            let sid = val_str(&s["stream_id"]);
            if sid.is_empty() {
                continue;
            }
            out.push(ParsedChannel {
                name: val_str(&s["name"]),
                url: format!("{base}/live/{u}/{p}/{sid}.ts"),
                group: cat_map
                    .get(&val_str(&s["category_id"]))
                    .cloned()
                    .unwrap_or_default(),
                tvg_id: val_str(&s["epg_channel_id"]),
                tvg_logo: val_str(&s["stream_icon"]),
            });
        }
    }
    if out.is_empty() {
        return Err("provider returned no live channels".into());
    }
    Ok(out)
}
