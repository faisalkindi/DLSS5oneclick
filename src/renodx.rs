//! RenoDX game-specific HDR mods: find the one for a game, install it beside DLSS 5.
//!
//! Sources (verified 2026-09-02):
//! - `games-index.json` on the RenoDX `snapshot` release: every mod built from
//!   clshortfuse/renodx main, with Steam app ids and artifact names.
//! - The wiki `Mods.md`: per-game download link (often a maintainer's fork
//!   snapshot, fresher than the main build), status (✅ / 🚧) and hover note.
//!
//! Coexistence with the DLSS 5 add-on is by design: ReShade only refuses two
//! add-ons exporting the same NAME; game mods export "RenoDX", the DLSS 5
//! add-on exports "DLSS 5 Neural Rendering", and their ReShade.ini sections
//! differ (`[renodx-preset1]` vs `[RenoDX.DLSS5]`). Two *game* mods do clash
//! (same NAME, same keys) — so exactly one per folder.

use crate::net;
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use serde_json::Value;
use std::fs;
use std::path::Path;

pub const GAMES_INDEX_URL: &str =
    "https://github.com/clshortfuse/renodx/releases/download/snapshot/games-index.json";
pub const SNAPSHOT_DOWNLOAD: &str =
    "https://github.com/clshortfuse/renodx/releases/download/snapshot/";
pub const WIKI_MODS_URL: &str = "https://raw.githubusercontent.com/wiki/clshortfuse/renodx/Mods.md";

/// The mod matched to a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mod {
    pub title: String,
    /// `renodx-<id>.addon64`
    pub file: String,
    /// Direct download.
    pub url: Option<String>,
    /// "stable", "wip" or "unknown" (not on the wiki).
    pub status: &'static str,
    pub note: String,
}

impl Mod {
    /// Who publishes the file about to be downloaded, shown before it is.
    pub fn source_label(&self) -> String {
        match self.url.as_deref() {
            Some(u) if u.starts_with(SNAPSHOT_DOWNLOAD) => "clshortfuse snapshot build".to_owned(),
            Some(u) => format!("{}'s fork", url_owner(u)),
            None => "no download".to_owned(),
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.status {
            "stable" => "working",
            "wip" => "in progress — may have issues",
            _ => "status unknown",
        }
    }
}

/// Letters and digits only, lower-case, bracketed suffixes dropped:
/// "Atlas Fallen (DX12)" and "Atlas Fallen" both become "atlasfallen".
pub fn norm_title(s: &str) -> String {
    let no_link = Regex::new(r"\[([^\]]+)\]\([^)]*\)")
        .unwrap()
        .replace_all(s, "$1");
    let no_paren = Regex::new(r"\([^)]*\)").unwrap().replace_all(&no_link, "");
    no_paren
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Steam app id for a game folder: walk up to `steamapps`, read the
/// `appmanifest_*.acf` whose `installdir` names the folder under `common`.
pub fn steam_appid(game_dir: &Path) -> Option<u64> {
    let common_child: String;
    let mut cur = game_dir;
    loop {
        let parent = cur.parent()?;
        if parent
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("common"))
            && parent
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n.eq_ignore_ascii_case("steamapps"))
        {
            common_child = cur.file_name()?.to_string_lossy().into_owned();
            break;
        }
        cur = parent;
    }
    let installdir = common_child;
    let steamapps = cur.parent()?.parent()?;
    let re_dir = Regex::new(r#""installdir"\s+"([^"]+)""#).unwrap();
    let re_id = Regex::new(r#""appid"\s+"(\d+)""#).unwrap();
    for e in fs::read_dir(steamapps).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
            continue;
        }
        let Ok(text) = fs::read_to_string(e.path()) else {
            continue;
        };
        if re_dir
            .captures(&text)
            .is_some_and(|c| c[1].eq_ignore_ascii_case(&installdir))
        {
            return re_id.captures(&text)?[1].parse().ok();
        }
    }
    None
}

/// One wiki row.
#[derive(Debug, Clone)]
pub struct WikiRow {
    pub title: String,
    pub url: Option<String>,
    pub status: &'static str,
    pub note: String,
}

/// Parse the wiki's mod table (rows are `| Name | Maintainer | Links | Status |`).
/// Where a wiki link may point before this tool will download from it: a
/// RenoDX fork's own GitHub Pages (`https://<owner>.github.io/renodx/…`) or
/// a fork's GitHub release (`https://github.com/<owner>/renodx/releases/download/…`),
/// https only. The wiki is editable by accounts outside the project, and a
/// link there used to be taken as-is and loaded into the game process (#102).
/// Anything else falls back to the project's own snapshot build.
pub fn wiki_host_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let owner_ok =
        |o: &str| !o.is_empty() && o.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if let Some(owner) = host.strip_suffix(".github.io") {
        return owner_ok(owner) && path.starts_with("renodx/");
    }
    if host == "github.com" {
        let mut parts = path.split('/');
        let owner = parts.next().unwrap_or("");
        return owner_ok(owner)
            && parts.next() == Some("renodx")
            && parts.next() == Some("releases")
            && parts.next() == Some("download");
    }
    false
}

/// The owner a resolved URL belongs to, for showing before the download:
/// `clshortfuse` for the project's own builds, the fork owner otherwise.
pub fn url_owner(url: &str) -> String {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if let Some(owner) = host.to_ascii_lowercase().strip_suffix(".github.io") {
        return owner.to_owned();
    }
    path.split('/').next().unwrap_or("").to_owned()
}

pub fn parse_wiki(md: &str) -> Vec<WikiRow> {
    let re_url = Regex::new(r"\((https?://[^)\s]+\.addon64)\)").unwrap();
    let re_note = Regex::new(r#"\(# "([^"]+)"\)"#).unwrap();
    md.lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| Name") && !l.starts_with("| :"))
        .filter_map(|l| {
            let title = l.split('|').nth(1)?.trim().to_owned();
            if title.is_empty() {
                return None;
            }
            let status = if l.contains(":white_check_mark:") {
                "stable"
            } else if l.contains(":construction:") {
                "wip"
            } else {
                "unknown"
            };
            Some(WikiRow {
                title,
                url: re_url.captures(l).map(|c| c[1].to_owned()),
                status,
                note: re_note
                    .captures_iter(l)
                    .map(|c| c[1].to_owned())
                    .collect::<Vec<_>>()
                    .join(" "),
            })
        })
        .collect()
}

/// Match a game against `games-index.json` by Steam app id, then by title vs
/// the folder name / exe stem. Returns (title, x64 artifact name).
pub fn match_index(
    index: &Value,
    appid: Option<u64>,
    names: &[String],
) -> Option<(String, String)> {
    let games = index.get("games")?.as_array()?;
    let x64_artifact = |g: &Value| -> Option<String> {
        let mods = g.get("mods")?.as_array()?;
        // Plain builds first, "Alternate"/"SDR" variants only when nothing else exists.
        let mut ordered: Vec<&Value> = mods.iter().filter(|m| m["variant"].is_null()).collect();
        ordered.extend(mods.iter().filter(|m| !m["variant"].is_null()));
        ordered
            .iter()
            .flat_map(|m| m["artifacts"].as_array().into_iter().flatten())
            .find(|a| a["arch"].as_str() == Some("x64"))
            .and_then(|a| a["name"].as_str())
            .map(str::to_owned)
    };
    let title = |g: &Value| {
        g.get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    if let Some(id) = appid {
        if let Some(g) = games
            .iter()
            .find(|g| g.get("steam_appid").and_then(Value::as_u64) == Some(id))
        {
            return Some((title(g), x64_artifact(g)?));
        }
    }
    let wanted: Vec<String> = names
        .iter()
        .map(|n| norm_title(n))
        .filter(|n| !n.is_empty())
        .collect();
    for g in games {
        let t = norm_title(&title(g));
        let exe_match = g
            .pointer("/deploy/game_exe")
            .and_then(Value::as_str)
            .map(|e| norm_title(e.trim_end_matches(".exe")))
            .is_some_and(|e| wanted.contains(&e));
        if (!t.is_empty() && wanted.contains(&t)) || exe_match {
            return Some((title(g), x64_artifact(g)?));
        }
    }
    None
}

/// Combine the index match with the wiki row of the same title.
pub fn resolve(index: &Value, wiki: &[WikiRow], game_dir: &Path, exe: &Path) -> Option<Mod> {
    let names: Vec<String> = [
        game_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        exe.file_stem().map(|n| n.to_string_lossy().into_owned()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let (title, file) = match match_index(index, steam_appid(game_dir), &names) {
        Some(hit) => hit,
        None => {
            // Not every mod has index metadata (Dragon's Dogma 2 is wiki-only):
            // match the wiki row by folder / exe name and take its file.
            let wanted: Vec<String> = names.iter().map(|n| norm_title(n)).collect();
            let r = wiki.iter().find(|r| {
                r.url.as_deref().is_some_and(wiki_host_allowed)
                    && wanted.contains(&norm_title(&r.title))
            })?;
            let u = r.url.as_deref()?;
            (r.title.clone(), net::file_name(u).to_owned())
        }
    };
    let key = norm_title(&title);
    let row = wiki.iter().find(|r| norm_title(&r.title) == key);
    Some(match row {
        Some(r) => {
            // The wiki link (often a maintainer fork) wins; a Nexus/Discord-only
            // row still installs the main-repo snapshot build, and says so.
            let (url, file, note) = match r.url.as_deref().filter(|u| wiki_host_allowed(u)) {
                Some(u) => (u.to_owned(), net::file_name(u).to_owned(), r.note.clone()),
                None if r.url.is_some() => (
                    format!("{SNAPSHOT_DOWNLOAD}{file}"),
                    file,
                    format!("{} The wiki's link points outside GitHub's renodx forks; the project's snapshot build is installed instead.", r.note).trim().to_owned(),
                ),
                None => (
                    format!("{SNAPSHOT_DOWNLOAD}{file}"),
                    file,
                    format!("{} Wiki lists this mod on Nexus/Discord; the snapshot build is installed instead.", r.note).trim().to_owned(),
                ),
            };
            Mod {
                title,
                file,
                url: Some(url),
                status: r.status,
                note,
            }
        }
        None => Mod {
            title,
            url: Some(format!("{SNAPSHOT_DOWNLOAD}{file}")),
            file,
            status: "unknown",
            note: String::new(),
        },
    })
}

/// Network lookup for one game.
pub fn lookup(client: &Client, exe: &Path) -> Result<Option<Mod>> {
    let dir = exe.parent().context("exe has no parent")?;
    let index: Value = serde_json::from_str(&net::get_text(client, GAMES_INDEX_URL)?)
        .context("games-index.json is not valid JSON")?;
    // The wiki is best-effort: status and fork links; the index alone still installs.
    let wiki = net::get_text(client, WIKI_MODS_URL)
        .map(|md| parse_wiki(&md))
        .unwrap_or_default();
    Ok(resolve(&index, &wiki, dir, exe))
}

/// Other RenoDX *game* mods already in the folder (not the DLSS 5 add-on, not
/// the DLSS fix, not the one this tool recorded). ReShade would refuse a second
/// "RenoDX" add-on and both would write the same ReShade.ini keys.
pub fn foreign_mods(dir: &Path, ours: Option<&str>) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("renodx-")
                && (l.ends_with(".addon64") || l.ends_with(".addon32"))
                && l != crate::game::DLSS5_ADDON
                && !l.starts_with("renodx-dlssfix")
                && !l.starts_with("renodx-dlss5")
                && !l.starts_with("renodx-dlss.")
                && ours.is_none_or(|o| !o.eq_ignore_ascii_case(n))
        })
        .collect();
    v.sort();
    v
}

/// Download the matched mod beside the exe and record it for Remove.
pub fn install(
    client: &Client,
    exe: &Path,
    m: &Mod,
    progress: net::Progress,
) -> Result<Vec<String>> {
    let dir = exe.parent().context("exe has no parent")?;
    let url = m.url.as_deref().ok_or_else(|| {
        anyhow!(
            "the RenoDX mod for {} is only published on Nexus Mods / Discord; download it by hand and drop the .addon64 next to the game exe",
            m.title
        )
    })?;
    let foreign = foreign_mods(dir, Some(&m.file));
    if !foreign.is_empty() {
        anyhow::bail!(
            "this game already has a RenoDX mod ({}); ReShade loads only one \"RenoDX\" add-on and two would fight over the same settings. Remove it first.",
            foreign.join(", ")
        );
    }
    let dest = dir.join(&m.file);
    if dest.is_file() {
        progress(100, "RenoDX mod already installed");
    } else {
        net::download(
            client,
            url,
            &dest,
            &format!("RenoDX: {}", m.title),
            progress,
        )?;
    }
    fs::write(dir.join(crate::game::RENODX_MANIFEST), &m.file)?;
    Ok(vec![format!(
        "{} ({}, {})",
        m.file,
        m.title,
        m.status_label()
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn norm_title_drops_marks_and_brackets() {
        assert_eq!(
            norm_title("Assassin’s Creed® Origins"),
            "assassinscreedorigins"
        );
        assert_eq!(norm_title("Atlas Fallen (DX12)"), "atlasfallen");
        assert_eq!(
            norm_title("[Crimson Desert](https://github.com/x/discussions/535)"),
            "crimsondesert"
        );
    }

    #[test]
    fn wiki_rows_parse_url_status_note() {
        let md = "| Name | Maintainer | Links | Status |\n| :--- | :-- | :-- | :-: |\n\
| Atlas Fallen (DX12) | Akuru | [![Snapshot](x)](https://clshortfuse.github.io/renodx/renodx-atlasfallen.addon64) | [:white_check_mark:](# \"Install ReShade in 'Atlas Fallen\\bin'. FSR2 causes crashes.\") |\n\
| Avowed | Ritsu | [![Nexus Mods](y)](https://www.nexusmods.com/avowed/mods/101) | :construction: |\n";
        let rows = parse_wiki(md);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].url.as_deref(),
            Some("https://clshortfuse.github.io/renodx/renodx-atlasfallen.addon64")
        );
        assert_eq!(rows[0].status, "stable");
        assert!(rows[0].note.starts_with("Install ReShade in"));
        assert_eq!(rows[1].url, None);
        assert_eq!(rows[1].status, "wip");
    }

    fn index() -> Value {
        json!({"games":[
            {"title":"Atlas Fallen (DX12)","steam_appid":1230530,"mods":[{"variant":null,"artifacts":[
                {"name":"renodx-atlasfallen.addon64","arch":"x64"}]}]},
            {"title":"Hollow Knight: Silksong","steam_appid":1030300,"deploy":{"game_exe":"Hollow Knight Silksong.exe"},
             "mods":[{"variant":null,"artifacts":[{"name":"renodx-hollowknight-silksong.addon64","arch":"x64"}]}]},
            {"title":"Alien: Isolation","steam_appid":214490,"mods":[{"variant":null,"artifacts":[
                {"name":"renodx-alienisolation.addon32","arch":"x86"}]}]}
        ]})
    }

    #[test]
    fn match_by_appid_then_folder_then_exe_and_skips_x86_only() {
        let ix = index();
        assert_eq!(
            match_index(&ix, Some(1230530), &[]).unwrap().1,
            "renodx-atlasfallen.addon64"
        );
        assert_eq!(
            match_index(&ix, None, &["Atlas Fallen".into()]).unwrap().1,
            "renodx-atlasfallen.addon64"
        );
        assert_eq!(
            match_index(&ix, None, &["Hollow Knight Silksong".into()])
                .unwrap()
                .1,
            "renodx-hollowknight-silksong.addon64"
        );
        assert!(match_index(&ix, Some(214490), &[]).is_none());
        assert!(match_index(&ix, None, &["Doom".into()]).is_none());
    }

    #[test]
    fn resolve_prefers_wiki_link_and_falls_back_to_snapshot() {
        let ix = index();
        let wiki = vec![WikiRow {
            title: "Atlas Fallen (DX12)".into(),
            url: Some("https://akuru-q.github.io/renodx/renodx-atlasfallen.addon64".into()),
            status: "stable",
            note: "bin folder".into(),
        }];
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Atlas Fallen");
        fs::create_dir(&dir).unwrap();
        let m = resolve(&ix, &wiki, &dir, &dir.join("AtlasFallen.exe")).unwrap();
        assert_eq!(
            m.url.as_deref(),
            Some("https://akuru-q.github.io/renodx/renodx-atlasfallen.addon64")
        );
        assert_eq!(m.status, "stable");
        let m = resolve(&ix, &[], &dir, &dir.join("AtlasFallen.exe")).unwrap();
        assert_eq!(
            m.url.as_deref(),
            Some("https://github.com/clshortfuse/renodx/releases/download/snapshot/renodx-atlasfallen.addon64")
        );
        assert_eq!(m.status, "unknown");
    }

    #[test]
    fn resolve_falls_back_to_wiki_only_rows() {
        let wiki = vec![WikiRow {
            title: "Dragon's Dogma 2".into(),
            url: Some("https://oopydoopy.github.io/renodx/renodx-dragonsdogma2.addon64".into()),
            status: "stable",
            note: "Delete shader.cache2".into(),
        }];
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Dragons Dogma 2");
        fs::create_dir(&dir).unwrap();
        let m = resolve(&index(), &wiki, &dir, &dir.join("DD2.exe")).unwrap();
        assert_eq!(m.file, "renodx-dragonsdogma2.addon64");
        assert_eq!(m.status, "stable");
        assert!(resolve(
            &index(),
            &wiki,
            &tmp.path().join("Nope"),
            &dir.join("x.exe")
        )
        .is_none());
    }

    #[test]
    fn steam_appid_reads_manifest_for_installdir() {
        let tmp = tempfile::tempdir().unwrap();
        let sa = tmp.path().join("steamapps");
        let game = sa.join("common").join("Atlas Fallen").join("bin");
        fs::create_dir_all(&game).unwrap();
        fs::write(
            sa.join("appmanifest_1230530.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1230530\"\n\t\"installdir\"\t\t\"Atlas Fallen\"\n}\n",
        )
        .unwrap();
        assert_eq!(steam_appid(&game), Some(1230530));
        assert_eq!(steam_appid(tmp.path()), None);
    }

    #[test]
    fn foreign_mods_ignores_dlss5_and_ours() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "renodx-dlss5.addon64",
            "renodx-dlssfix.addon64",
            "renodx-cp2077.addon64",
            "renodx-ff7rebirth.addon64",
        ] {
            fs::write(tmp.path().join(f), b"x").unwrap();
        }
        assert_eq!(
            foreign_mods(tmp.path(), Some("renodx-cp2077.addon64")),
            vec!["renodx-ff7rebirth.addon64".to_string()]
        );
    }

    /// The wiki is editable by outsiders; only GitHub renodx forks are
    /// followed, https only, and anything else drops to the snapshot (#102).
    #[test]
    fn wiki_links_are_allowlisted() {
        assert!(wiki_host_allowed(
            "https://clshortfuse.github.io/renodx/renodx-x.addon64"
        ));
        assert!(wiki_host_allowed(
            "https://Some-Fork.github.io/renodx/mods/renodx-x.addon64"
        ));
        assert!(wiki_host_allowed(
            "https://github.com/someone/renodx/releases/download/v1/renodx-x.addon64"
        ));
        assert!(!wiki_host_allowed(
            "http://clshortfuse.github.io/renodx/renodx-x.addon64"
        ));
        assert!(!wiki_host_allowed(
            "https://evil.example/renodx/renodx-x.addon64"
        ));
        assert!(!wiki_host_allowed(
            "https://evil.github.io/other/renodx-x.addon64"
        ));
        assert!(!wiki_host_allowed(
            "https://github.com/someone/other/releases/download/v1/x.addon64"
        ));
        assert!(!wiki_host_allowed(
            "https://github.com/someone/renodx/raw/main/x.addon64"
        ));
        assert!(!wiki_host_allowed(
            "https://evil.github.io.example/renodx/x.addon64"
        ));
        assert_eq!(
            url_owner("https://clshortfuse.github.io/renodx/a.addon64"),
            "clshortfuse"
        );
        assert_eq!(
            url_owner("https://github.com/fork/renodx/releases/download/v1/a.addon64"),
            "fork"
        );

        let md = "| Atlas Fallen (DX12) | Akuru | [![Snapshot](x)](https://evil.example/renodx-atlasfallen.addon64) | [:white_check_mark:](# \"ok\") |\n";
        let wiki = parse_wiki(md);
        let ix = index();
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Atlas Fallen");
        fs::create_dir(&d).unwrap();
        let m = resolve(&ix, &wiki, &d, &d.join("AtlasFallen.exe")).unwrap();
        assert_eq!(
            m.url.as_deref(),
            Some("https://github.com/clshortfuse/renodx/releases/download/snapshot/renodx-atlasfallen.addon64")
        );
        assert!(m.note.contains("outside GitHub"), "{}", m.note);
        assert_eq!(m.source_label(), "clshortfuse snapshot build");
    }
}
