//! M3U / M3U8 playlist parsing.
//!
//! Handles the common extended-M3U format used by IPTV playlists (e.g. iptv-org):
//!
//! ```text
//! #EXTM3U
//! #EXTINF:-1 tvg-id="..." tvg-logo="..." group-title="News;Public",Channel Name (1080p)
//! https://example.com/stream.m3u8
//! ```
//!
//! Unknown `#`-directives (`#EXTVLCOPT`, `#EXTGRP`, comments) are tolerated.

use std::collections::HashMap;

/// A single parsed channel entry (pre-database, no id yet).
#[derive(Debug, Clone)]
pub struct ParsedChannel {
    pub name: String,
    pub url: String,
    pub group: String,
    pub tvg_id: String,
    pub tvg_logo: String,
}

/// Parse an extended-M3U document into channel entries.
pub fn parse_m3u(content: &str) -> Vec<ParsedChannel> {
    let mut out = Vec::new();
    let mut pending: Option<Extinf> = None;
    // `#EXTGRP:` can set the group for the following entry when group-title is absent.
    let mut pending_group: Option<String> = None;

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(parse_extinf(rest));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTGRP:") {
            pending_group = Some(rest.trim().to_string());
            continue;
        }
        if line.starts_with('#') {
            // Other directive/comment — ignore.
            continue;
        }
        // A non-comment line is the URL for the most recent #EXTINF.
        if let Some(info) = pending.take() {
            let group = if !info.group.is_empty() {
                info.group
            } else {
                pending_group.take().unwrap_or_default()
            };
            out.push(ParsedChannel {
                name: info.name,
                url: line.to_string(),
                group,
                tvg_id: info.tvg_id,
                tvg_logo: info.tvg_logo,
            });
        }
        pending_group = None;
    }
    out
}

struct Extinf {
    name: String,
    group: String,
    tvg_id: String,
    tvg_logo: String,
}

/// Parse the part after `#EXTINF:` — `<duration> key="val"...,Display Name`.
fn parse_extinf(rest: &str) -> Extinf {
    // The display name is everything after the first comma that is not inside quotes.
    let mut in_quotes = false;
    let mut split_at = None;
    for (i, ch) in rest.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                split_at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (attrs_part, name) = match split_at {
        Some(i) => (&rest[..i], rest[i + 1..].trim().to_string()),
        None => (rest, String::new()),
    };
    let attrs = parse_attrs(attrs_part);
    Extinf {
        name,
        group: attrs.get("group-title").cloned().unwrap_or_default(),
        tvg_id: attrs.get("tvg-id").cloned().unwrap_or_default(),
        tvg_logo: attrs.get("tvg-logo").cloned().unwrap_or_default(),
    }
}

/// Extract `key="value"` attribute pairs from an attribute string.
fn parse_attrs(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut i = 0;
    while let Some(rel) = s[i..].find("=\"") {
        let key_end = i + rel;
        // Key is the token immediately before '=', back to the previous whitespace.
        let key_start = s[..key_end]
            .rfind(char::is_whitespace)
            .map(|p| p + 1)
            .unwrap_or(0);
        let key = s[key_start..key_end].to_string();
        let val_start = key_end + 2; // skip `="`
        match s[val_start..].find('"') {
            Some(q) => {
                let val = s[val_start..val_start + q].to_string();
                if !key.is_empty() {
                    map.insert(key, val);
                }
                i = val_start + q + 1;
            }
            None => break,
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_entry() {
        let m3u = "#EXTM3U\n#EXTINF:-1 tvg-id=\"a.b\" tvg-logo=\"http://x/y.png\" group-title=\"News;Public\",Foo News (1080p)\nhttp://s/stream.m3u8\n";
        let c = parse_m3u(m3u);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "Foo News (1080p)");
        assert_eq!(c[0].url, "http://s/stream.m3u8");
        assert_eq!(c[0].group, "News;Public");
        assert_eq!(c[0].tvg_id, "a.b");
    }

    #[test]
    fn handles_comma_in_name() {
        let m3u = "#EXTINF:-1 group-title=\"X\",Hello, World\nhttp://s\n";
        let c = parse_m3u(m3u);
        assert_eq!(c[0].name, "Hello, World");
        assert_eq!(c[0].group, "X");
    }
}
