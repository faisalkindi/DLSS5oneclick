//! Installed-game scan: Steam, Epic, GOG, Xbox (Game Pass), EA app, Ubisoft
//! Connect. Newest install first.
//!
//! Sources (verified 2026-09-02):
//! - Steam: `<Steam>\steamapps\libraryfolders.vdf` lists library roots; each has
//!   `steamapps\appmanifest_<appid>.acf` with `name` / `installdir`. Posters are
//!   cached by the client at `<Steam>\appcache\librarycache\<appid>\library_600x900.jpg`,
//!   sometimes one hash-named folder deeper; the CDN serves the same file at
//!   `cdn.cloudflare.steamstatic.com/steam/apps/<appid>/library_600x900.jpg`.
//! - Epic: `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item` (JSON:
//!   DisplayName, InstallLocation, LaunchExecutable, bIsApplication).
//! - GOG: `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<id>` (gameName, path, exe).
//! - Xbox: `<drive>:\.GamingRoot` = "RGBX" + u32 + UTF-16 folder name (XboxGames);
//!   each game is `<folder>\<Game>\Content\MicrosoftGame.config` (ExecutableList,
//!   ShellVisuals DefaultDisplayName / Square150x150Logo / StoreLogo).
//! - EA app: `HKLM\SOFTWARE\WOW6432Node\EA Games\<game>` "Install Dir", and the
//!   `__Installer\installerdata.xml` every EA app install carries (gameTitle).
//!   Game Pass titles from EA install through the EA app, not into XboxGames.
//! - Ubisoft Connect: `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>`
//!   "InstallDir", and the `uplay_install.state` file in the game folder.
//!   Game Pass titles from Ubisoft land in XboxGames with no `Content` folder.
//! - The two marker files are also looked for one folder below each drive root,
//!   its XboxGames folder and the launchers' default folders, so an install the
//!   launcher no longer lists (moved drive, new PC) still shows.
//!
//! Install date = the game folder's creation time (every store, one rule).

use regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Store {
    Steam,
    Epic,
    Gog,
    Xbox,
    Ea,
    Ubisoft,
    /// Pointed at by hand and remembered (#28).
    Manual,
}

impl Store {
    pub fn label(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
            Store::Gog => "GOG",
            Store::Xbox => "Xbox",
            Store::Ea => "EA app",
            Store::Ubisoft => "Ubisoft Connect",
            Store::Manual => "Added by you",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poster {
    /// A jpeg/png on disk.
    File(PathBuf),
    /// Steam CDN, cached under `%LOCALAPPDATA%\dlss5oneclick\posters\steam_<appid>.jpg`.
    SteamCdn(u64),
    /// No artwork anywhere: the launch exe's icon.
    ExeIcon(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Game {
    pub title: String,
    pub store: Store,
    /// Folder handed to the tool (resolved to the real exe by `game::resolve_target`).
    pub dir: PathBuf,
    /// Exe the store names; `None` when the manifest does not say.
    pub exe_hint: Option<PathBuf>,
    pub installed: SystemTime,
    pub poster: Poster,
}

// ── Steam ──────────────────────────────────────────────────────────

fn steam_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(p) = reg::read_sz(reg::HKCU, r"Software\Valve\Steam", "SteamPath") {
            let p = PathBuf::from(p.replace('/', "\\"));
            if p.is_dir() {
                return Some(p);
            }
        }
        if let Some(p) = reg::read_sz(
            reg::HKLM,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ) {
            let p = PathBuf::from(p);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    let p = PathBuf::from(r"C:\Program Files (x86)\Steam");
    p.is_dir().then_some(p)
}

/// Library roots from libraryfolders.vdf (the Steam root itself is one of them).
pub fn steam_library_roots(vdf: &str) -> Vec<PathBuf> {
    Regex::new(r#""path"\s+"([^"]+)""#)
        .unwrap()
        .captures_iter(vdf)
        .map(|c| PathBuf::from(c[1].replace("\\\\", "\\")))
        .collect()
}

/// Which app ids libraryfolders.vdf lists under each library root. A game moved
/// between drives can leave its old `appmanifest` behind; the library Steam
/// lists the app under is the one it uses.
pub fn steam_library_apps(vdf: &str) -> Vec<(PathBuf, Vec<u64>)> {
    let path_re = Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
    let app_re = Regex::new(r#""(\d+)"\s+"\d+""#).unwrap();
    let marks: Vec<(usize, usize, PathBuf)> = path_re
        .captures_iter(vdf)
        .map(|c| {
            let m = c.get(0).unwrap();
            (
                m.start(),
                m.end(),
                PathBuf::from(c[1].replace("\\\\", "\\")),
            )
        })
        .collect();
    let mut out = Vec::new();
    for (i, (_, end, root)) in marks.iter().enumerate() {
        let stop = marks.get(i + 1).map_or(vdf.len(), |m| m.0);
        let seg = &vdf[*end..stop];
        let ids = seg
            .find("\"apps\"")
            .map(|at| {
                app_re
                    .captures_iter(&seg[at..])
                    .filter_map(|c| c[1].parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        out.push((root.clone(), ids));
    }
    out
}

/// Of several copies of one Steam game, the one to show: the copy in the
/// library Steam lists the app under, else the one updated last.
/// `listed[i]` says copy `i` sits in such a library; `updated[i]` is its
/// `LastUpdated`.
pub fn choose_steam_copy(listed: &[bool], updated: &[u64]) -> usize {
    if let Some(i) = listed.iter().position(|&l| l) {
        return i;
    }
    updated
        .iter()
        .enumerate()
        .max_by_key(|(_, u)| **u)
        .map_or(0, |(i, _)| i)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Acf {
    pub appid: u64,
    pub name: String,
    pub installdir: String,
}

pub fn parse_acf(text: &str) -> Option<Acf> {
    let grab = |k: &str| -> Option<String> {
        Regex::new(&format!(r#""{k}"\s+"([^"]*)""#))
            .unwrap()
            .captures(text)
            .map(|c| c[1].to_owned())
    };
    Some(Acf {
        appid: grab("appid")?.parse().ok()?,
        name: grab("name")?,
        installdir: grab("installdir")?,
    })
}

/// Steam entries that are runtimes and redistributables, never games.
fn steam_is_tool(a: &Acf) -> bool {
    let n = a.name.to_ascii_lowercase();
    a.appid == 228980
        || n.contains("steamworks common redistributables")
        || n.starts_with("proton")
        || n.contains("steam linux runtime")
        || n.contains("steamvr")
}

/// Portrait art first (`library_600x900.jpg`, or the newer `library_capsule.jpg`),
/// then landscape (`library_header.jpg`, `header.jpg`), in the app folder or one
/// hash-named folder below it. Nothing local: the CDN.
fn steam_poster(steam: &Path, appid: u64) -> Poster {
    let dir = steam
        .join("appcache")
        .join("librarycache")
        .join(appid.to_string());
    let mut places = vec![dir.clone()];
    if let Ok(rd) = fs::read_dir(&dir) {
        places.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    for name in [
        "library_600x900.jpg",
        "library_capsule.jpg",
        "library_header.jpg",
        "header.jpg",
    ] {
        for place in &places {
            let p = place.join(name);
            if p.is_file() {
                return Poster::File(p);
            }
        }
    }
    Poster::SteamCdn(appid)
}

fn scan_steam(out: &mut Vec<Game>) {
    let Some(steam) = steam_root() else { return };
    let vdf =
        fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")).unwrap_or_default();
    let mut roots = steam_library_roots(&vdf);
    if !roots.iter().any(|r| r == &steam) {
        roots.push(steam.clone());
    }
    let listed = steam_library_apps(&vdf);
    let norm = |p: &Path| p.to_string_lossy().to_ascii_lowercase();
    // (app id, copy listed by Steam, LastUpdated, the game) for every copy.
    let mut copies: Vec<(u64, bool, u64, Game)> = Vec::new();
    let updated_re = Regex::new(r#""LastUpdated"\s+"(\d+)""#).unwrap();
    for root in roots {
        let sa = root.join("steamapps");
        let Ok(rd) = fs::read_dir(&sa) else { continue };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if !(n.starts_with("appmanifest_") && n.ends_with(".acf")) {
                continue;
            }
            let Some(a) = fs::read_to_string(e.path())
                .ok()
                .and_then(|t| parse_acf(&t))
            else {
                continue;
            };
            if steam_is_tool(&a) {
                continue;
            }
            let dir = sa.join("common").join(&a.installdir);
            if !dir.is_dir() {
                continue;
            }
            let is_listed = listed
                .iter()
                .any(|(r, ids)| norm(r) == norm(&root) && ids.contains(&a.appid));
            let updated = fs::read_to_string(e.path())
                .ok()
                .and_then(|t| updated_re.captures(&t).and_then(|c| c[1].parse().ok()))
                .unwrap_or(0);
            copies.push((
                a.appid,
                is_listed,
                updated,
                Game {
                    title: a.name.clone(),
                    store: Store::Steam,
                    installed: created(&dir),
                    poster: steam_poster(&steam, a.appid),
                    exe_hint: None,
                    dir,
                },
            ));
        }
    }
    // A game left in two libraries (moved between drives, old manifest kept)
    // is one game: show the copy Steam uses.
    let mut seen: Vec<u64> = Vec::new();
    for i in 0..copies.len() {
        let id = copies[i].0;
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let same: Vec<usize> = (0..copies.len()).filter(|&j| copies[j].0 == id).collect();
        let pick = choose_steam_copy(
            &same.iter().map(|&j| copies[j].1).collect::<Vec<_>>(),
            &same.iter().map(|&j| copies[j].2).collect::<Vec<_>>(),
        );
        out.push(copies[same[pick]].3.clone());
    }
}

// ── Epic ───────────────────────────────────────────────────────────

pub fn parse_epic_item(json: &str) -> Option<(String, PathBuf, Option<PathBuf>)> {
    let v: Value = serde_json::from_str(json).ok()?;
    if v["bIsApplication"] != Value::Bool(true) || v["bIsIncompleteInstall"] == Value::Bool(true) {
        return None;
    }
    let title = v["DisplayName"].as_str()?.to_owned();
    let dir = PathBuf::from(v["InstallLocation"].as_str()?);
    let exe = v["LaunchExecutable"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| dir.join(s.replace('/', "\\")));
    Some((title, dir, exe))
}

fn scan_epic(out: &mut Vec<Game>) {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let dir = base
        .join("Epic")
        .join("EpicGamesLauncher")
        .join("Data")
        .join("Manifests");
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if !e
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(".item")
        {
            continue;
        }
        let Some((title, dir, exe)) = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| parse_epic_item(&t))
        else {
            continue;
        };
        if !dir.is_dir() {
            continue;
        }
        let icon = exe.clone().filter(|p| p.is_file());
        out.push(Game {
            title,
            store: Store::Epic,
            installed: created(&dir),
            poster: icon
                .map(Poster::ExeIcon)
                .unwrap_or_else(|| Poster::ExeIcon(dir.clone())),
            exe_hint: exe,
            dir,
        });
    }
}

// ── GOG ────────────────────────────────────────────────────────────

#[cfg(windows)]
fn scan_gog(out: &mut Vec<Game>) {
    let base = r"SOFTWARE\WOW6432Node\GOG.com\Games";
    for id in reg::subkeys(reg::HKLM, base) {
        let key = format!(r"{base}\{id}");
        let (Some(name), Some(path)) = (
            reg::read_sz(reg::HKLM, &key, "gameName"),
            reg::read_sz(reg::HKLM, &key, "path"),
        ) else {
            continue;
        };
        let dir = PathBuf::from(path);
        if !dir.is_dir() {
            continue;
        }
        let exe = reg::read_sz(reg::HKLM, &key, "exe")
            .map(PathBuf::from)
            .filter(|p| p.is_file());
        out.push(Game {
            title: name,
            store: Store::Gog,
            installed: created(&dir),
            poster: Poster::ExeIcon(exe.clone().unwrap_or_else(|| dir.clone())),
            exe_hint: exe,
            dir,
        });
    }
}

#[cfg(not(windows))]
fn scan_gog(_out: &mut Vec<Game>) {}

// ── Xbox / Game Pass ───────────────────────────────────────────────

/// `.GamingRoot`: "RGBX", a u32, then the games folder name in UTF-16 (NUL-terminated).
#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn parse_gaming_root(b: &[u8]) -> Option<String> {
    if b.len() < 10 || &b[..4] != b"RGBX" {
        return None;
    }
    let units: Vec<u16> = b[8..]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    let s = String::from_utf16_lossy(&units);
    (!s.is_empty()).then_some(s)
}

#[derive(Debug, PartialEq, Eq)]
pub struct XboxConfig {
    pub title: Option<String>,
    pub exe: Option<String>,
    pub logo: Option<String>,
}

pub fn parse_microsoft_game_config(xml: &str) -> XboxConfig {
    let grab = |re: &str| {
        Regex::new(re)
            .unwrap()
            .captures(xml)
            .map(|c| c[1].to_owned())
    };
    let title = grab(r#"DefaultDisplayName="([^"]+)""#).filter(|t| !t.starts_with("ms-resource"));
    XboxConfig {
        title,
        exe: grab(r#"<Executable[^>]*\sName="([^"]+)""#),
        logo: grab(r#"Square150x150Logo="([^"]+)""#).or_else(|| grab(r#"StoreLogo="([^"]+)""#)),
    }
}

fn scan_xbox(out: &mut Vec<Game>) {
    for letter in b'A'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        let Ok(bytes) = fs::read(root.join(".GamingRoot")) else {
            continue;
        };
        let Some(folder) = parse_gaming_root(&bytes) else {
            continue;
        };
        let Ok(rd) = fs::read_dir(root.join(&folder)) else {
            continue;
        };
        for e in rd.flatten() {
            let content = e.path().join("Content");
            let Ok(xml) = fs::read_to_string(content.join("MicrosoftGame.config")) else {
                continue;
            };
            let cfg = parse_microsoft_game_config(&xml);
            let title = cfg
                .title
                .unwrap_or_else(|| e.file_name().to_string_lossy().into_owned());
            let exe = cfg.exe.map(|x| content.join(x.replace('/', "\\")));
            let poster = match cfg
                .logo
                .map(|l| content.join(l.replace('/', "\\")))
                .filter(|p| p.is_file())
            {
                Some(p) => Poster::File(p),
                None => Poster::ExeIcon(exe.clone().unwrap_or_else(|| content.clone())),
            };
            out.push(Game {
                title,
                store: Store::Xbox,
                installed: created(&content),
                poster,
                exe_hint: exe,
                dir: content,
            });
        }
    }
}

// ── EA app / Ubisoft Connect ───────────────────────────────────────

const EA_MARKER: &str = r"__Installer\installerdata.xml";
const UBI_MARKER: &str = "uplay_install.state";

/// The English title from an EA `installerdata.xml` (else the first one), without
/// the trademark signs.
pub fn parse_ea_title(xml: &str) -> Option<String> {
    let grab = |re: &str| {
        Regex::new(re)
            .unwrap()
            .captures(xml)
            .map(|c| c[1].to_owned())
    };
    let t = grab(r#"<gameTitle[^>]*locale="en_US"[^>]*>([^<]+)<"#)
        .or_else(|| grab(r"<gameTitle[^>]*>([^<]+)<"))?;
    let t = t.replace(['\u{2122}', '\u{00AE}'], "").trim().to_owned();
    (!t.is_empty()).then_some(t)
}

/// EA or Ubisoft game in `dir`, known by the launcher's own marker file.
fn launcher_game(dir: &Path) -> Option<Game> {
    let (store, title) = if let Ok(xml) = fs::read_to_string(dir.join(EA_MARKER)) {
        (Store::Ea, parse_ea_title(&xml))
    } else if dir.join(UBI_MARKER).is_file() {
        (Store::Ubisoft, None)
    } else {
        return None;
    };
    let exe = crate::game::resolve_target(dir).ok().map(|(e, _)| e);
    // Ubisoft keeps no title on disk. A folder with spaces is a readable name
    // ("Assassin's Creed Shadows", whose exe is ACShadows_Plus.exe); one without
    // is not, and then the exe usually is ("The Rogue Prince of Persia.exe" in
    // "TheRoguePrinceOfPersia").
    let folder = dir.file_name().map(|s| s.to_string_lossy().into_owned());
    let stem = exe
        .as_ref()
        .and_then(|e| e.file_stem())
        .map(|s| s.to_string_lossy().into_owned());
    let title = title
        .or_else(|| folder.clone().filter(|f| f.contains(' ')))
        .or(stem)
        .or(folder)?;
    Some(Game {
        title,
        store,
        installed: created(dir),
        poster: Poster::ExeIcon(exe.clone().unwrap_or_else(|| dir.to_path_buf())),
        exe_hint: exe,
        dir: dir.to_path_buf(),
    })
}

/// Folders a launcher install can sit in: what the launchers' registry keys
/// name, plus every folder one level below the places people and launchers put
/// games on each drive.
fn launcher_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        for (base, value) in [
            (r"SOFTWARE\WOW6432Node\EA Games", "Install Dir"),
            (
                r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs",
                "InstallDir",
            ),
        ] {
            for k in reg::subkeys(reg::HKLM, base) {
                if let Some(p) = reg::read_sz(reg::HKLM, &format!(r"{base}\{k}"), value) {
                    dirs.push(PathBuf::from(p.replace('/', "\\").trim_end_matches('\\')));
                }
            }
        }
    }
    for letter in b'A'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if !root.is_dir() {
            continue;
        }
        let mut parents = vec![
            root.clone(),
            root.join("EA Games"),
            root.join("Games"),
            root.join(r"Program Files\EA Games"),
            root.join(r"Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games"),
        ];
        if let Some(f) = fs::read(root.join(".GamingRoot"))
            .ok()
            .and_then(|b| parse_gaming_root(&b))
        {
            parents.push(root.join(f));
        }
        for parent in parents {
            if let Ok(rd) = fs::read_dir(&parent) {
                dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
    }
    dirs
}

fn scan_launchers(out: &mut Vec<Game>) {
    out.extend(
        launcher_candidates()
            .iter()
            .filter_map(|d| launcher_game(d)),
    );
}

// ── common ─────────────────────────────────────────────────────────

fn created(p: &Path) -> SystemTime {
    fs::metadata(p)
        .and_then(|m| m.created().or_else(|_| m.modified()))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Every installed game, newest install first, one entry per folder.
pub fn scan() -> Vec<Game> {
    let mut v = Vec::new();
    scan_steam(&mut v);
    scan_epic(&mut v);
    scan_gog(&mut v);
    scan_xbox(&mut v);
    scan_launchers(&mut v);
    // Last: a hand-added path that a store also lists loses to the store entry
    // in sort_and_dedupe, which keeps the first of each folder.
    scan_added(&mut v);
    sort_and_dedupe(&mut v);
    v
}

// -- added by hand ---------------------------------------------------

/// Paths the user pointed at, one per line. Written next to the poster cache.
pub fn added_list_file() -> PathBuf {
    poster_cache_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("added-games.txt")
}

pub fn added_paths() -> Vec<PathBuf> {
    fs::read_to_string(added_list_file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn write_added(v: &[PathBuf]) {
    let f = added_list_file();
    if let Some(d) = f.parent() {
        let _ = fs::create_dir_all(d);
    }
    let body = v
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = fs::write(f, body);
}

/// Remember a path the user picked (a folder or an exe). No duplicates.
pub fn remember_added(path: &Path) {
    let mut v = added_paths();
    let same = |a: &Path| {
        a.to_string_lossy().to_ascii_lowercase() == path.to_string_lossy().to_ascii_lowercase()
    };
    if v.iter().any(|p| same(p)) {
        return;
    }
    v.push(path.to_path_buf());
    write_added(&v);
}

pub fn forget_added(path: &Path) {
    let key = path.to_string_lossy().to_ascii_lowercase();
    let mut v = added_paths();
    // The list holds what was picked (maybe an exe); the card knows only the folder.
    v.retain(|p| {
        let s = p.to_string_lossy().to_ascii_lowercase();
        s != key
            && p.parent().map(|d| d.to_string_lossy().to_ascii_lowercase()) != Some(key.clone())
    });
    write_added(&v);
}

/// One `Game` per remembered path; a path that no longer exists is dropped.
pub fn added_games(paths: &[PathBuf]) -> Vec<Game> {
    let mut v = Vec::new();
    for p in paths {
        let (dir, exe_hint) = if p.is_file() {
            (p.parent().unwrap_or(p).to_path_buf(), Some(p.clone()))
        } else if p.is_dir() {
            (
                p.clone(),
                crate::game::resolve_target(p).ok().map(|(e, _)| e),
            )
        } else {
            continue;
        };
        let title = if p.is_file() {
            p.file_stem()
        } else {
            p.file_name()
        }
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
        let installed = fs::metadata(&dir)
            .and_then(|m| m.created())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let poster = Poster::ExeIcon(exe_hint.clone().unwrap_or_else(|| dir.clone()));
        v.push(Game {
            title,
            store: Store::Manual,
            dir,
            exe_hint,
            installed,
            poster,
        });
    }
    v
}

fn scan_added(v: &mut Vec<Game>) {
    v.extend(added_games(&added_paths()));
}

pub fn sort_and_dedupe(v: &mut Vec<Game>) {
    v.sort_by(|a, b| {
        b.installed
            .cmp(&a.installed)
            .then_with(|| a.title.cmp(&b.title))
    });
    let mut seen = std::collections::HashSet::new();
    v.retain(|g| seen.insert(g.dir.to_string_lossy().to_ascii_lowercase()));
}

/// Where downloaded posters live.
pub fn poster_cache_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dlss5oneclick")
        .join("posters")
}

pub const STEAM_CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps/";

/// Decode the poster to RGBA. Downloads Steam CDN art once; falls back to the exe icon.
pub fn poster_rgba(client: &reqwest::blocking::Client, p: &Poster) -> Option<image::RgbaImage> {
    match p {
        Poster::File(path) => image::open(path).ok().map(|i| i.to_rgba8()),
        Poster::SteamCdn(appid) => {
            let cache = poster_cache_dir();
            let file = cache.join(format!("steam_{appid}.jpg"));
            let none = cache.join(format!("steam_{appid}.none"));
            if none.is_file() {
                return None;
            }
            if !file.is_file() {
                let fetch = |url: &str| -> Option<Vec<u8>> {
                    client
                        .get(url)
                        .send()
                        .ok()?
                        .error_for_status()
                        .ok()?
                        .bytes()
                        .ok()
                        .map(|b| b.to_vec())
                };
                // Older titles: fixed path. Newer ones: only the store API knows the
                // hashed asset URL (landscape header; the card letterboxes it).
                let bytes = fetch(&format!("{STEAM_CDN}{appid}/library_600x900.jpg"))
                    .or_else(|| {
                        let url = format!(
                            "https://store.steampowered.com/api/appdetails?appids={appid}&filters=basic"
                        );
                        let v: Value = serde_json::from_slice(&fetch(&url)?).ok()?;
                        let header = v[appid.to_string()]["data"]["header_image"].as_str()?;
                        fetch(header)
                    });
                fs::create_dir_all(&cache).ok()?;
                match bytes {
                    Some(b) => fs::write(&file, &b).ok()?,
                    None => {
                        let _ = fs::write(&none, b"");
                        return None;
                    }
                }
            }
            image::open(&file).ok().map(|i| i.to_rgba8())
        }
        Poster::ExeIcon(path) => icon::exe_icon_rgba(path),
    }
}

#[cfg(windows)]
/// The Windows compatibility layer set on `exe` (Properties ▸ Compatibility, or
/// a store's own shim), from `AppCompatFlags\Layers` under HKCU then HKLM. A
/// shimmed process reports an older Windows to NGX, whose capability query then
/// answers PlatformError before touching any device (DLSS5-Feeder#47).
#[cfg(windows)]
pub fn compat_layer(exe: &Path) -> Option<String> {
    const SUB: &str = r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers";
    let name = exe.to_string_lossy();
    [reg::HKCU, reg::HKLM]
        .into_iter()
        .find_map(|root| reg::read_sz(root, SUB, &name))
        .filter(|v| !v.trim().is_empty())
}
#[cfg(not(windows))]
pub fn compat_layer(_exe: &Path) -> Option<String> {
    None
}

mod reg {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ,
    };
    pub const HKLM: HKEY = HKEY_LOCAL_MACHINE;
    pub const HKCU: HKEY = HKEY_CURRENT_USER;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn read_sz(root: HKEY, sub: &str, name: &str) -> Option<String> {
        let sub_w = wide(sub);
        let name_w = wide(name);
        let mut buf = [0u16; 1024];
        let mut size: u32 = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                root,
                sub_w.as_ptr(),
                name_w.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut _,
                &mut size,
            )
        };
        if rc != 0 {
            return None;
        }
        let n = (size as usize / 2).saturating_sub(1).min(buf.len());
        Some(
            String::from_utf16_lossy(&buf[..n])
                .trim_end_matches('\0')
                .to_owned(),
        )
    }

    pub fn subkeys(root: HKEY, sub: &str) -> Vec<String> {
        let sub_w = wide(sub);
        let mut key: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(root, sub_w.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
            return vec![];
        }
        let mut out = Vec::new();
        for i in 0..4096u32 {
            let mut name = [0u16; 256];
            let mut len: u32 = name.len() as u32;
            let rc = unsafe {
                RegEnumKeyExW(
                    key,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if rc != 0 {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        unsafe { RegCloseKey(key) };
        out
    }
}

mod icon {
    use std::path::Path;

    /// 256 px shell icon of an exe (or a folder), as RGBA.
    #[cfg(windows)]
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn exe_icon_rgba(path: &Path) -> Option<image::RgbaImage> {
        use windows_sys::Win32::Graphics::Gdi::{
            DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            DIB_RGB_COLORS,
        };
        use windows_sys::Win32::UI::Shell::SHDefExtractIconW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DestroyIcon, GetIconInfo, HICON, ICONINFO,
        };

        let wide: Vec<u16> = path.as_os_str().encode_wide_lossy();
        let mut hicon: HICON = std::ptr::null_mut();
        const SIZE: u32 = 256;
        let hr = unsafe {
            SHDefExtractIconW(wide.as_ptr(), 0, 0, &mut hicon, std::ptr::null_mut(), SIZE)
        };
        if hr != 0 || hicon.is_null() {
            return None;
        }
        let mut info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(hicon, &mut info) } == 0 {
            unsafe { DestroyIcon(hicon) };
            return None;
        }
        let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = SIZE as i32;
        bmi.bmiHeader.biHeight = -(SIZE as i32); // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bgra = vec![0u8; (SIZE * SIZE * 4) as usize];
        let dc = unsafe { GetDC(std::ptr::null_mut()) };
        let lines = unsafe {
            GetDIBits(
                dc,
                info.hbmColor,
                0,
                SIZE,
                bgra.as_mut_ptr() as *mut _,
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        unsafe {
            ReleaseDC(std::ptr::null_mut(), dc);
            if !info.hbmColor.is_null() {
                DeleteObject(info.hbmColor as _);
            }
            if !info.hbmMask.is_null() {
                DeleteObject(info.hbmMask as _);
            }
            DestroyIcon(hicon);
        }
        if lines == 0 {
            return None;
        }
        let mut rgba = bgra;
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        // An icon without an alpha channel comes back fully transparent: treat as opaque.
        if rgba.chunks_exact(4).all(|p| p[3] == 0) {
            for px in rgba.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        image::RgbaImage::from_raw(SIZE, SIZE, rgba)
    }

    #[cfg(not(windows))]
    pub fn exe_icon_rgba(_path: &Path) -> Option<image::RgbaImage> {
        None
    }

    #[cfg(windows)]
    trait EncodeWideLossy {
        fn encode_wide_lossy(&self) -> Vec<u16>;
    }
    #[cfg(windows)]
    impl EncodeWideLossy for std::ffi::OsStr {
        fn encode_wide_lossy(&self) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            self.encode_wide().chain(std::iter::once(0)).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_vdf_and_acf_parse() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}\n";
        assert_eq!(
            steam_library_roots(vdf),
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary")
            ]
        );
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"1903340\"\n\t\"name\"\t\t\"Clair Obscur: Expedition 33\"\n\t\"installdir\"\t\t\"Expedition 33\"\n}\n";
        let a = parse_acf(acf).unwrap();
        assert_eq!(a.appid, 1903340);
        assert_eq!(a.installdir, "Expedition 33");
        assert!(!steam_is_tool(&a));
        assert!(steam_is_tool(&Acf {
            appid: 228980,
            name: "Steamworks Common Redistributables".into(),
            installdir: "x".into()
        }));
    }

    #[test]
    fn epic_item_parse() {
        let j = r#"{"bIsApplication":true,"bIsIncompleteInstall":false,"DisplayName":"Nuclear Throne","InstallLocation":"D:\\Epic Games\\NuclearThrone","LaunchExecutable":"nuclearthrone.exe"}"#;
        let (t, d, e) = parse_epic_item(j).unwrap();
        assert_eq!(t, "Nuclear Throne");
        assert_eq!(d, PathBuf::from(r"D:\Epic Games\NuclearThrone"));
        assert_eq!(
            e,
            Some(PathBuf::from(
                r"D:\Epic Games\NuclearThrone\nuclearthrone.exe"
            ))
        );
        assert!(parse_epic_item(
            r#"{"bIsApplication":false,"DisplayName":"UE","InstallLocation":"x"}"#
        )
        .is_none());
    }

    /// #28: a path picked by hand survives a restart, and Forget removes it
    /// whether the card knows the exe or only the folder.
    #[test]
    fn added_games_round_trip() {
        let tmp = std::env::temp_dir().join("dlss5oneclick-test-added");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("Bin")).unwrap();
        let exe = tmp.join("Bin").join("Game.exe");
        fs::write(&exe, b"MZ").unwrap();

        let list = added_games(&[tmp.clone(), exe.clone()]);
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|g| g.store == Store::Manual));
        assert_eq!(list[0].title, "dlss5oneclick-test-added");
        assert_eq!(list[1].title, "Game");
        assert_eq!(list[1].dir, tmp.join("Bin"));
        assert_eq!(list[1].exe_hint.as_deref(), Some(exe.as_path()));

        // A path that no longer exists is dropped, not shown as a dead card.
        assert!(added_games(&[tmp.join("gone")]).is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn xbox_gaming_root_and_config_parse() {
        let mut b = b"RGBX\x01\x00\x00\x00".to_vec();
        for u in "XboxGames".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b.extend_from_slice(&[0, 0]);
        assert_eq!(parse_gaming_root(&b).as_deref(), Some("XboxGames"));
        assert_eq!(parse_gaming_root(b"nope"), None);
        let xml = r#"<Game configVersion="1"><ExecutableList><Executable Name="ExampleGame.exe" Id="Game"/></ExecutableList>
<ShellVisuals DefaultDisplayName="Example Game" Square150x150Logo="GraphicsLogo.png" StoreLogo="StoreLogo.png"/></Game>"#;
        let c = parse_microsoft_game_config(xml);
        assert_eq!(c.title.as_deref(), Some("Example Game"));
        assert_eq!(c.exe.as_deref(), Some("ExampleGame.exe"));
        assert_eq!(c.logo.as_deref(), Some("GraphicsLogo.png"));
        let c = parse_microsoft_game_config(
            r#"<ShellVisuals DefaultDisplayName="ms-resource:Title" StoreLogo="s.png"/>"#,
        );
        assert_eq!(c.title, None);
        assert_eq!(c.logo.as_deref(), Some("s.png"));
    }

    /// A game left in two libraries is shown once, from the library Steam
    /// lists the app under (else the copy updated last).
    #[test]
    fn a_game_in_two_steam_libraries_shows_the_copy_steam_uses() {
        let vdf = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\Steam"
		"apps"
		{
			"10"		"100"
		}
	}
	"1"
	{
		"path"		"F:\SteamLibrary"
		"apps"
		{
			"1263240"		"11819991095"
			"20"		"5"
		}
	}
	"2"
	{
		"path"		"H:\SteamLibrary"
		"label"		""
	}
}"#;
        let apps = steam_library_apps(vdf);
        assert_eq!(apps.len(), 3);
        assert_eq!(apps[1].0, PathBuf::from(r"F:\SteamLibrary"));
        assert_eq!(apps[1].1, vec![1263240, 20]);
        assert!(apps[2].1.is_empty());
        // Copy 0 sits in a library that lists the app: it wins even if older.
        assert_eq!(choose_steam_copy(&[false, true], &[900, 100]), 1);
        // Neither listed: the most recently updated copy.
        assert_eq!(choose_steam_copy(&[false, false], &[100, 900]), 1);
        assert_eq!(choose_steam_copy(&[true], &[0]), 0);
    }

    #[test]
    fn newest_first_and_deduped() {
        let g = |t: &str, dir: &str, secs: u64| Game {
            title: t.into(),
            store: Store::Steam,
            dir: PathBuf::from(dir),
            exe_hint: None,
            installed: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs),
            poster: Poster::SteamCdn(1),
        };
        let mut v = vec![
            g("Old", r"C:\a", 10),
            g("New", r"C:\b", 30),
            g("Dup", r"c:\B", 20),
        ];
        sort_and_dedupe(&mut v);
        let titles: Vec<&str> = v.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["New", "Old"]);
    }

    #[test]
    fn ea_title_is_the_english_one_without_trademark_signs() {
        let xml = "<gameTitles>\n<gameTitle locale=\"fr_FR\">Dead Space\u{2122} FR</gameTitle>\n<gameTitle locale=\"en_US\">Dead Space\u{2122}</gameTitle>\n</gameTitles>";
        assert_eq!(parse_ea_title(xml).as_deref(), Some("Dead Space"));
        assert_eq!(
            parse_ea_title("<gameTitle locale=\"de_DE\">Mass Effect\u{00AE}</gameTitle>")
                .as_deref(),
            Some("Mass Effect")
        );
        assert_eq!(parse_ea_title("<gameTitles></gameTitles>"), None);
    }

    /// EA and Ubisoft installs are found by the launcher's own marker file,
    /// with no registry entry, and nothing else is taken for one.
    #[test]
    fn launcher_markers_are_recognised() {
        let t = tempfile::tempdir().unwrap();
        let ea = t.path().join("Dead Space (2023)");
        fs::create_dir_all(ea.join("__Installer")).unwrap();
        fs::write(
            ea.join(EA_MARKER),
            "<gameTitle locale=\"en_US\">Dead Space\u{2122}</gameTitle>",
        )
        .unwrap();
        let g = launcher_game(&ea).unwrap();
        assert_eq!((g.store, g.title.as_str()), (Store::Ea, "Dead Space"));

        let ubi = t.path().join("TheRoguePrinceOfPersia");
        fs::create_dir_all(&ubi).unwrap();
        fs::write(ubi.join(UBI_MARKER), b"x").unwrap();
        assert_eq!(launcher_game(&ubi).unwrap().store, Store::Ubisoft);

        let plain = t.path().join("Something");
        fs::create_dir_all(&plain).unwrap();
        assert!(launcher_game(&plain).is_none());
    }
}
