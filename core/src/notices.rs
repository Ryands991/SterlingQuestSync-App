//! Notices the server owner posts ("Manual update needed", "Maintenance tonight"), from /v1/meta.
//! The server already checks them; the app checks every field again and picks the ones meant for
//! its own version, so a broken or hostile server can't show markup, odd characters or outside links.

use serde::{Deserialize, Serialize};

pub const MAX_SHOWN: usize = 3;
const MAX_TEXT: usize = 240;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Notice {
    pub id: String,
    pub text: String,
    /// info (gold), warn (amber) or bad (red).
    pub level: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

fn version(v: &str) -> Option<[u32; 3]> {
    let mut out = [0u32; 3];
    let mut parts = v.split('.');
    for slot in out.iter_mut() {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// Links may only open Sterling's own pages.
pub fn link_ok(link: &str) -> bool {
    let rest = if let Some(r) = link.strip_prefix("https://github.com/SterlingAddons/") {
        r
    } else if let Some(r) = link.strip_prefix("https://sterlingaddons.com/") {
        r
    } else {
        return false;
    };
    rest.len() <= 160 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"._~/?#=&%+-".contains(&b))
}

fn id_ok(id: &str) -> bool {
    (1..=24).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn text_ok(t: &str) -> bool {
    let n = t.chars().count();
    (1..=MAX_TEXT).contains(&n) && !t.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

/// Is this notice well formed and meant for `app_version` right now?
pub fn applies(n: &Notice, app_version: &str, now: i64) -> bool {
    let Some(me) = version(app_version) else { return false };
    if !id_ok(&n.id) || !text_ok(&n.text) || !matches!(n.level.as_str(), "info" | "warn" | "bad") {
        return false;
    }
    if n.link.as_deref().is_some_and(|l| !link_ok(l)) {
        return false;
    }
    if n.until.is_some_and(|u| u <= now) {
        return false;
    }
    for (bound, low) in [(&n.from, true), (&n.to, false)] {
        if let Some(b) = bound {
            let Some(b) = version(b) else { return false };
            if (low && me < b) || (!low && me > b) {
                return false;
            }
        }
    }
    true
}

/// The notices to show: for this version, not dismissed, at most MAX_SHOWN, no repeated ids.
pub fn for_app(list: &[Notice], app_version: &str, dismissed: &[String], now: i64) -> Vec<Notice> {
    let mut out: Vec<Notice> = Vec::new();
    for n in list {
        if out.len() >= MAX_SHOWN {
            break;
        }
        if applies(n, app_version, now) && !dismissed.contains(&n.id) && !out.iter().any(|o| o.id == n.id) {
            out.push(n.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str) -> Notice {
        Notice {
            id: id.into(),
            text: "Manual update needed: download 0.1.24 from the app's page.".into(),
            level: "bad".into(),
            from: None,
            to: Some("0.1.23".into()),
            link: Some("https://github.com/SterlingAddons/SterlingQuestSync-App/releases/latest".into()),
            until: None,
        }
    }

    #[test]
    fn picks_by_version_range() {
        assert!(applies(&n("a"), "0.1.23", 0));
        assert!(applies(&n("a"), "0.1.9", 0));
        assert!(!applies(&n("a"), "0.1.24", 0));
        assert!(!applies(&n("a"), "0.10.0", 0));
        let mut x = n("a");
        x.from = Some("0.1.25".into());
        x.to = None;
        assert!(!applies(&x, "0.1.24", 0));
        assert!(applies(&x, "0.1.25", 0));
        assert!(applies(&x, "1.0.0", 0));
    }

    #[test]
    fn refuses_anything_odd() {
        for link in [
            "https://github.com/SterlingAddonsX/a",
            "https://github.com/SterlingAddons.evil.example/a",
            "https://sterlingaddons.com.evil.example/",
            "http://github.com/SterlingAddons/a",
            "https://github.com/SterlingAddons/a b",
            "https://github.com/SterlingAddons/a\"&calc",
            "file:///C:/Windows",
        ] {
            let mut x = n("a");
            x.link = Some(link.into());
            assert!(!applies(&x, "0.1.20", 0), "{link}");
        }
        for text in ["", "<b>hi</b>", "line\nbreak", &"x".repeat(241)] {
            let mut x = n("a");
            x.text = text.into();
            assert!(!applies(&x, "0.1.20", 0));
        }
        for (f, v) in [("id", "BAD ID"), ("level", "urgent"), ("to", "0.1"), ("to", "v0.1.2")] {
            let mut x = n("a");
            match f {
                "id" => x.id = v.into(),
                "level" => x.level = v.into(),
                _ => x.to = Some(v.into()),
            }
            assert!(!applies(&x, "0.1.20", 0), "{f}={v}");
        }
    }

    #[test]
    fn expired_dismissed_repeated_and_too_many_are_left_out() {
        let mut old = n("old");
        old.until = Some(100);
        let list = vec![old, n("a"), n("a"), n("b"), n("c"), n("d"), n("e")];
        let shown = for_app(&list, "0.1.20", &["b".into()], 200);
        let ids: Vec<_> = shown.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "d"]);
    }

    #[test]
    fn reads_the_servers_shape() {
        let raw = r#"[{"id":"m1","text":"Hi","level":"info"},{"id":"m2","text":"Old","level":"warn","to":"0.1.23","until":5}]"#;
        let list: Vec<Notice> = serde_json::from_str(raw).unwrap();
        assert_eq!(for_app(&list, "0.1.25", &[], 1).len(), 1);
    }
}
