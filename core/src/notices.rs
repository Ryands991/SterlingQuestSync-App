//! Notices the server owner posts ("Manual update needed", "Maintenance tonight"), from /v1/meta.
//! The server already checks them; the app checks every field again and picks the ones meant for
//! its own version, so a broken or hostile server can't show markup, odd characters or outside links.

use serde::{Deserialize, Serialize};

pub const MAX_SHOWN: usize = 3;
const MAX_TEXT: usize = 240;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Notice
{
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

/// "0.1.25" as three numbers.
fn version(text: &str) -> Option<[u32; 3]>
{
    let mut out = [0u32; 3];
    let mut parts = text.split('.');

    for slot in out.iter_mut()
    {
        let part = parts.next()?;

        if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }

        *slot = part.parse().ok()?;
    }

    if parts.next().is_some()
    {
        return None;
    }

    Some(out)
}

/// Links may only open Sterling's own pages.
pub fn link_ok(link: &str) -> bool
{
    let rest = if let Some(tail) = link.strip_prefix("https://github.com/SterlingAddons/")
    {
        tail
    }

    else if let Some(tail) = link.strip_prefix("https://sterlingaddons.com/")
    {
        tail
    }

    else
    {
        return false;
    };

    rest.len() <= 160 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"._~/?#=&%+-".contains(&b))
}

/// A notice id: short, lowercase letters, digits and dashes.
fn id_ok(id: &str) -> bool
{
    (1..=24).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Notice text that's safe and short enough to show.
fn text_ok(text: &str) -> bool
{
    let count = text.chars().count();
    (1..=MAX_TEXT).contains(&count) && !text.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

/// Is this notice well formed and meant for `app_version` right now?
pub fn applies(notice: &Notice, app_version: &str, now: i64) -> bool
{
    let Some(me) = version(app_version) else { return false };

    if !id_ok(&notice.id) || !text_ok(&notice.text) || !matches!(notice.level.as_str(), "info" | "warn" | "bad")
    {
        return false;
    }

    if notice.link.as_deref().is_some_and(|l| !link_ok(l))
    {
        return false;
    }

    if notice.until.is_some_and(|u| u <= now)
    {
        return false;
    }

    for (bound, low) in [(&notice.from, true), (&notice.to, false)]
    {
        if let Some(limit_text) = bound
        {
            let Some(limit) = version(limit_text) else { return false };

            if (low && me < limit) || (!low && me > limit)
            {
                return false;
            }
        }
    }

    true
}

/// The notices to show: for this version, not dismissed, at most MAX_SHOWN, no repeated ids.
pub fn for_app(list: &[Notice], app_version: &str, dismissed: &[String], now: i64) -> Vec<Notice>
{
    let mut out: Vec<Notice> = Vec::new();

    for notice in list
    {
        if out.len() >= MAX_SHOWN
        {
            break;
        }

        if applies(notice, app_version, now) && !dismissed.contains(&notice.id) && !out.iter().any(|o| o.id == notice.id)
        {
            out.push(notice.clone());
        }
    }

    out
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn n(id: &str) -> Notice
    {
        Notice
        {
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
    fn picks_by_version_range()
    {
        assert!(applies(&n("a"), "0.1.23", 0));
        assert!(applies(&n("a"), "0.1.9", 0));
        assert!(!applies(&n("a"), "0.1.24", 0));
        assert!(!applies(&n("a"), "0.10.0", 0));

        let mut notice = n("a");

        notice.from = Some("0.1.25".into());
        notice.to = None;
        assert!(!applies(&notice, "0.1.24", 0));
        assert!(applies(&notice, "0.1.25", 0));
        assert!(applies(&notice, "1.0.0", 0));
    }

    #[test]
    fn refuses_anything_odd()
    {
        for link in
        [
            "https://github.com/SterlingAddonsX/a",
            "https://github.com/SterlingAddons.evil.example/a",
            "https://sterlingaddons.com.evil.example/",
            "http://github.com/SterlingAddons/a",
            "https://github.com/SterlingAddons/a b",
            "https://github.com/SterlingAddons/a\"&calc",
            "file:///C:/Windows",
        ]

        {
            let mut notice = n("a");

            notice.link = Some(link.into());
            assert!(!applies(&notice, "0.1.20", 0), "{link}");
        }

        for text in ["", "<b>hi</b>", "line\nbreak", &"x".repeat(241)]
        {
            let mut notice = n("a");

            notice.text = text.into();
            assert!(!applies(&notice, "0.1.20", 0));
        }

        for (field, value) in [("id", "BAD ID"), ("level", "urgent"), ("to", "0.1"), ("to", "v0.1.2")]
        {
            let mut notice = n("a");

            match field
            {
                "id" => notice.id = value.into(),
                "level" => notice.level = value.into(),
                _ => notice.to = Some(value.into()),
            }

            assert!(!applies(&notice, "0.1.20", 0), "{field}={value}");
        }
    }

    #[test]
    fn expired_dismissed_repeated_and_too_many_are_left_out()
    {
        let mut old = n("old");

        old.until = Some(100);

        let list = vec![old, n("a"), n("a"), n("b"), n("c"), n("d"), n("e")];
        let shown = for_app(&list, "0.1.20", &["b".into()], 200);
        let ids: Vec<_> = shown.iter().map(|x| x.id.as_str()).collect();

        assert_eq!(ids, ["a", "c", "d"]);
    }

    #[test]
    fn reads_the_servers_shape()
    {
        let raw = r#"[{"id":"m1","text":"Hi","level":"info"},{"id":"m2","text":"Old","level":"warn","to":"0.1.23","until":5}]"#;
        let list: Vec<Notice> = serde_json::from_str(raw).unwrap();

        assert_eq!(for_app(&list, "0.1.25", &[], 1).len(), 1);
    }
}
