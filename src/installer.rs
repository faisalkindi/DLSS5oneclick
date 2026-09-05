//! The six install steps, in the order the DLSS5-Feeder README lists them.
//!
//! Sources (verified 2026-08-31):
//! 1. ReShade add-on build — https://reshade.me links `/downloads/ReShade_Setup_<ver>_Addon.exe`;
//!    that exe has an appended ZIP with ReShade64.dll / ReShade32.dll. Dropped as dxgi.dll.
//! 2. ReShade shader headers — raw.githubusercontent.com/crosire/reshade-shaders/slim/Shaders/
//!    {ReShade.fxh, ReShadeUI.fxh, DrawText.fxh}; the setup exe only carries the DLLs.
//! 3. DLSS5-Feeder — jlrouzies-fr/DLSS5-Feeder latest release, loose assets
//!    `dlss5-feed.addon64` + `DLSS5_Feed.fx` (the `feed-vk-layer.zip` is Vulkan-only, unused).
//! 4. LumeniteFX — umar-afzaal/LumeniteFX branch `mainline` (no releases):
//!    Shaders/lumenite_*.fx, Shaders/include/*.fxh, Textures/lumenite_bluenoise256.png.
//! 5. DLSS 5 add-on — RankFTW/rhi-repo releases: `renodx-dlss5-*` (renodx-dlss5.addon64),
//!    `dlssnr-*` (nvngx_dlssnr.dll), `dlss-*` (nvngx_dlss.dll; not dlssg-/dlssd-).
//! 6. ReShade.ini + ReShadePreset.ini: DLSS5_MV_PROVIDER=3, Lumenite_Kernel above DLSS5_Feed.

use crate::game::{self, GameStatus};
use crate::gpupref;
use crate::net::{self, Progress};
use crate::renodx;
use crate::reshade_ini;
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const RESHADE_HOME: &str = "https://reshade.me";
pub const RESHADE_SHADERS_RAW: &str =
    "https://raw.githubusercontent.com/crosire/reshade-shaders/slim/Shaders/";
pub const FEEDER_REPO: &str = "jlrouzies-fr/DLSS5-Feeder";
pub const LUMENITE_ZIP: &str =
    "https://codeload.github.com/umar-afzaal/LumeniteFX/zip/refs/heads/mainline";

/// Which install engine carries the DLSS 5 pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// ReShade + RenoDX add-on (both game kinds; the default).
    #[default]
    ReShade,
    /// Dagherbou's OptiScaler fork with the built-in Neural Rendering pass.
    /// Games with native DLSS only (the pass reads the inputs the game hands to DLSS).
    Opti,
}

pub const OPTI_RELEASES: &str = "https://api.github.com/repos/Dagherbou/OptiScaler_DLSSNR/releases";

const STEP_OPTI: Step = Step {
    name: "OptiScaler + DLSS Neural Rendering",
    run: step_opti,
};

/// Extract the whole OptiScaler_DLSSNR release into the game folder,
/// writing `OptiScaler.dll` as `dxgi.dll` (the fork's default load name for
/// DX11/DX12 games) and recording every path in a manifest for uninstall.
/// The release tag recorded in an OptiScaler manifest, from its `# tag v…`
/// header. A manifest written before this was recorded has none.
/// The newest release of every component, fetched once and compared against
/// what each game has recorded. Empty fields mean "could not check".
#[derive(Debug, Clone, Default)]
pub struct Latest {
    pub reshade: Option<String>,
    pub feeder: Option<String>,
    pub opti: Option<String>,
    pub dlss: Option<String>,
    pub dlssnr: Option<String>,
}

impl Latest {
    pub fn fetch(client: &Client) -> Self {
        Latest {
            reshade: resolve_reshade_setup(client).ok().map(|(v, _)| v),
            feeder: net::latest_tag(client, FEEDER_REPO).ok(),
            opti: net::latest_tag(client, OPTI_REPO).ok(),
            dlss: rhi_latest(client, "dlss-").ok().map(|(t, _)| t),
            dlssnr: rhi_latest(client, "dlssnr-").ok().map(|(t, _)| t),
        }
    }
}

/// Components this tool placed in `dir` whose recorded version is behind
/// `latest`. A component with no marker was not placed by this tool and is
/// never reported, so a user's own ReShade never shows up as "out of date".
pub fn stale_components(dir: &Path, latest: &Latest) -> Vec<String> {
    let mine = |marker: &str| fs::read_to_string(dir.join(marker)).ok();
    let mut out = Vec::new();
    let mut check = |name: &str, have: Option<String>, want: &Option<String>| {
        if let (Some(h), Some(w)) = (have, want) {
            if h.trim() != w.trim() {
                out.push(format!("{name} {} → {w}", h.trim()));
            }
        }
    };
    check("ReShade", mine(game::RESHADE_MARKER), &latest.reshade);
    check("DLSS5-Feeder", mine(game::FEEDER_MARKER), &latest.feeder);
    check("nvngx_dlss.dll", mine(game::DLSS_MARKER), &latest.dlss);
    check(
        "nvngx_dlssnr.dll",
        mine(game::DLSSNR_MARKER),
        &latest.dlssnr,
    );
    if let Ok(m) = fs::read_to_string(dir.join(game::OPTI_MANIFEST)) {
        match (manifest_tag(&m), &latest.opti) {
            (Some(have), Some(want)) if have.trim() != want.trim() => {
                out.push(format!("OptiScaler {} → {want}", have.trim()))
            }
            // Installed before the version was recorded (0.11.0), so what is on
            // disk cannot be compared: an Install settles it either way.
            (None, Some(want)) => out.push(format!("OptiScaler unknown version → {want}")),
            _ => {}
        }
    }
    out
}

fn manifest_tag(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("# tag "))
        .map(|t| t.trim().to_owned())
}

fn step_opti(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    if game::is_reshade_dll(&d.join(game::RESHADE_PROXY)) {
        bail!(
            "ReShade is installed as dxgi.dll in this game; OptiScaler needs that name. \
             Run Remove (or Remove incl. ReShade) first, then install with the OptiScaler engine."
        );
    }
    progress(0, "Looking up latest OptiScaler DLSS-NR release");
    // An installed OptiScaler used to be left alone forever, so a game set up
    // in August still ran August's build after every reinstall. The tag is
    // recorded in the manifest; a copy this tool placed is refreshed when
    // upstream moves on, and one it did not place is never touched.
    let latest = net::latest_tag(client, OPTI_REPO).ok();
    if st.opti {
        // No manifest at all: somebody else put OptiScaler there. A manifest
        // without a "# tag" line is ours, from before the tag was recorded --
        // refresh it, which also writes the tag for next time.
        let Some(manifest) = fs::read_to_string(d.join(game::OPTI_MANIFEST)).ok() else {
            return Ok(vec![
                "OptiScaler present (not placed by this tool, left as is)".to_owned(),
            ]);
        };
        match (manifest_tag(&manifest), &latest) {
            (Some(a), Some(b)) if &a == b => {
                return Ok(vec![format!("OptiScaler already current ({a})")]);
            }
            (Some(a), Some(b)) => progress(0, &format!("OptiScaler {a} is out, {b} available")),
            (Some(_), None) => {
                return Ok(vec![
                    "OptiScaler present (could not check for a newer one)".to_owned()
                ]);
            }
            (None, _) => progress(0, "OptiScaler version not recorded, refreshing"),
        }
    }
    // Stable release only (releases/latest skips pre-releases); the API list
    // and the releases page both put betas first.
    let asset: String = match latest.clone() {
        Some(tag) => net::github_asset_url_html(client, OPTI_REPO, &tag, r#"[^"]+\.zip"#)?,
        None => match net::get_json_github(client, OPTI_RELEASES) {
            Ok(releases) => releases
                .as_array()
                .and_then(|a| a.iter().find(|r| r["prerelease"] != Value::Bool(true)))
                .and_then(|r| r.get("assets"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|a| a.get("browser_download_url"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("OptiScaler_DLSSNR has no release asset"))?,
            Err(_) => {
                let tags = net::github_release_tags_html(client, OPTI_REPO, "v", 2)?;
                let tag = tags
                    .first()
                    .ok_or_else(|| anyhow!("no OptiScaler_DLSSNR release found"))?;
                net::github_asset_url_html(client, OPTI_REPO, tag, r#"[^"]+\.zip"#)?
            }
        },
    };
    let asset = asset.as_str();
    let zip_path = work.join("optiscaler-dlssnr.zip");
    net::download(client, asset, &zip_path, "OptiScaler DLSS-NR", progress)?;

    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("OptiScaler download is not a valid zip")?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let mut installed: Vec<String> = Vec::new();
    for member in names {
        // This zip uses backslash separators; normalise, and never trust the path.
        let rel = member.replace('\\', "/");
        if rel.ends_with('/') {
            continue;
        }
        let parts: Vec<&str> = rel
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        if parts.is_empty() {
            continue;
        }
        let fname = parts.last().unwrap().to_string();
        // The interactive setup script and its banner file are not needed:
        // the renaming it performs is done right here.
        if fname.eq_ignore_ascii_case("setup_windows.bat")
            || fname.eq_ignore_ascii_case("setup_linux.sh")
            || fname.starts_with("!!")
        {
            continue;
        }
        let out_rel = if fname.eq_ignore_ascii_case("OptiScaler.dll") {
            game::RESHADE_PROXY.to_string() // dxgi.dll
        } else {
            parts.join("/")
        };
        let dest = d.join(out_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        // A refresh must not overwrite the settings file. It carries the user's
        // choices -- upscaler, frame generation, LoadReshade -- and replacing it
        // silently turns them all back to auto, which is how a working RenoDX
        // install stopped loading ReShade after a routine update.
        if fname.eq_ignore_ascii_case(OPTI_INI) && dest.is_file() {
            installed.push(out_rel);
            continue;
        }
        net::extract_member(&mut zip, &member, &dest)?;
        installed.push(out_rel);
    }
    if !installed.iter().any(|p| p == game::RESHADE_PROXY) {
        bail!("the OptiScaler release had no OptiScaler.dll — layout changed upstream");
    }
    let header = latest
        .as_deref()
        .map(|t| format!("# tag {t}\n"))
        .unwrap_or_default();
    fs::write(
        d.join(game::OPTI_MANIFEST),
        format!("{header}{}", installed.join("\n")),
    )?;
    installed.push(game::OPTI_MANIFEST.into());
    Ok(installed)
}

/// Remove an OptiScaler install recorded in the manifest.
fn uninstall_opti(d: &Path, removed: &mut Vec<String>) -> Result<()> {
    let manifest = d.join(game::OPTI_MANIFEST);
    let Ok(list) = fs::read_to_string(&manifest) else {
        return Ok(());
    };
    for rel in list
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let clean: Vec<&str> = rel
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        let p = clean
            .iter()
            .fold(d.to_path_buf(), |acc, part| acc.join(part));
        if p.is_file() {
            fs::remove_file(&p)?;
            removed.push(rel.to_string());
        }
    }
    // Clean now-empty folders the archive created.
    for sub in ["OptiScaler/D3D12_OptiScaler", "OptiScaler", "Licenses"] {
        let p = d.join(sub.replace('/', std::path::MAIN_SEPARATOR_STR));
        if p.is_dir() && fs::read_dir(&p)?.next().is_none() {
            fs::remove_dir(&p)?;
        }
    }
    fs::remove_file(&manifest)?;
    removed.push(game::OPTI_MANIFEST.into());
    Ok(())
}

pub const BRIDGE_DOWNLOAD: &str =
    "https://github.com/NIGos/dlss5-bridge/releases/latest/download/dlss5-bridge.addon64";
pub const RHI_RELEASES: &str =
    "https://api.github.com/repos/RankFTW/rhi-repo/releases?per_page=100";
pub const RHI_REPO: &str = "RankFTW/rhi-repo";
pub const OPTI_REPO: &str = "Dagherbou/OptiScaler_DLSSNR";

#[derive(Clone, Copy)]
pub struct Step {
    pub name: &'static str,
    pub run: fn(&Client, &GameStatus, &Path, Progress) -> Result<Vec<String>>,
}

const STEP_RESHADE: Step = Step {
    name: "ReShade (add-on build)",
    run: step_reshade,
};
const STEP_HEADERS: Step = Step {
    name: "ReShade shader headers",
    run: step_headers,
};
const STEP_FEEDER: Step = Step {
    name: "DLSS5-Feeder",
    run: step_feeder,
};
const STEP_LUMENITE: Step = Step {
    name: "LumeniteFX motion vectors",
    run: step_lumenite,
};
const STEP_DLSS5: Step = Step {
    name: "DLSS 5 add-on + models",
    run: step_dlss5,
};
const STEP_DLSSNR_ONLY: Step = Step {
    name: "DLSS 5 model (nvngx_dlssnr.dll)",
    run: step_dlssnr_only,
};
const STEP_BRIDGE: Step = Step {
    name: "DLSS 5 DX11 bridge",
    run: step_bridge,
};
const STEP_CONFIG: Step = Step {
    name: "ReShade config",
    run: step_config,
};
const STEP_FEEDER_CLEANUP: Step = Step {
    name: "Remove DLSS5-Feeder (game has native DLSS)",
    run: step_feeder_cleanup,
};
const STEP_REFRAMEWORK: Step = Step {
    name: "REFramework (RE Engine needs it before ReShade)",
    run: step_reframework,
};
const STEP_RENODX: Step = Step {
    name: "RenoDX HDR mod for this game",
    run: step_renodx,
};
const STEP_HOST_RESHADE: Step = Step {
    name: "64-bit ReShade for the host64 helper",
    run: step_host_reshade,
};

/// 32-bit games: the helper process needs its own 64-bit ReShade as
/// `host64\dxgi.dll` (the Feeder README: "run the ReShade installer once
/// against any 64-bit game and take it from there"). Same marker/refresh rule
/// as the in-game copy.
fn step_host_reshade(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let host = st.consumer_dir();
    fs::create_dir_all(&host)?;
    progress(0, "Looking up latest ReShade");
    let (ver, url) = resolve_reshade_setup(client)?;
    if st.host_reshade {
        match fs::read_to_string(host.join(game::RESHADE_MARKER)) {
            Ok(mine) if mine.trim() == ver => {
                return Ok(vec![format!("host64/dxgi.dll already current ({ver})")]);
            }
            Ok(_) => progress(0, &format!("host64 ReShade {ver} is out, refreshing")),
            Err(_) => {
                return Ok(vec![
                    "host64/dxgi.dll present (not placed by this tool)".into()
                ])
            }
        }
    }
    let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
    net::download(client, &url, &setup, "ReShade (64-bit, host64)", progress)?;
    install_reshade_from_setup(&setup, &host, 64, game::RESHADE_PROXY)?;
    fs::write(host.join(game::RESHADE_MARKER), ver.as_bytes())?;
    Ok(vec![format!("{}/{}", game::HOST_DIR, game::RESHADE_PROXY)])
}

const STEP_GPU_PREF: Step = Step {
    name: "GPU preference",
    run: step_gpu_pref,
};
const STEP_RESHADE_VIA_OPTI: Step = Step {
    name: "ReShade loaded by OptiScaler (ReShade64.dll)",
    run: step_reshade_via_opti,
};

/// ReShade beside OptiScaler, the way OptiScaler.ini documents it: the ReShade
/// DLL as `ReShade64.dll` next to the exe and `[Plugins] LoadReshade=true`, so
/// OptiScaler (which holds dxgi.dll) loads it and ReShade add-ons still work.
fn step_reshade_via_opti(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let ini = d.join(OPTI_INI);
    if !ini.is_file() {
        bail!("{OPTI_INI} not found — install the OptiScaler engine first");
    }
    let mut done = Vec::new();
    let dll = d.join(RESHADE64);
    if !dll.is_file() {
        progress(0, "Looking up latest ReShade");
        let (ver, url) = resolve_reshade_setup(client)?;
        let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
        net::download(client, &url, &setup, "ReShade", progress)?;
        install_reshade_from_setup(&setup, d, st.bitness, RESHADE64)?;
        // Recorded in the OptiScaler manifest so Remove takes it out with the engine.
        let mut m = fs::read_to_string(d.join(game::OPTI_MANIFEST)).unwrap_or_default();
        if !m.lines().any(|l| l == RESHADE64) {
            if !m.is_empty() && !m.ends_with('\n') {
                m.push('\n');
            }
            m.push_str(RESHADE64);
            fs::write(d.join(game::OPTI_MANIFEST), m)?;
        }
        done.push(RESHADE64.to_owned());
    }
    let text = fs::read_to_string(&ini)?;
    if let Some(new) = set_load_reshade(&text) {
        fs::write(&ini, new)?;
        done.push(format!("{OPTI_INI}: LoadReshade=true"));
    }
    if done.is_empty() {
        progress(100, "ReShade64.dll + LoadReshade already set");
    }
    Ok(done)
}

pub const OPTI_INI: &str = "OptiScaler.ini";
pub const RESHADE64: &str = "ReShade64.dll";

/// `LoadReshade=true` in OptiScaler.ini; `None` when already set.
pub fn set_load_reshade(ini: &str) -> Option<String> {
    let mut out = String::with_capacity(ini.len() + 32);
    let mut seen = false;
    let mut changed = false;
    for line in ini.split_inclusive('\n') {
        let t = line.trim_end_matches(['\r', '\n']);
        let key = t.split('=').next().unwrap_or("").trim();
        if key.eq_ignore_ascii_case("LoadReshade") {
            seen = true;
            if t.split('=').nth(1).map(str::trim) != Some("true") {
                out.push_str("LoadReshade=true");
                out.push_str(&line[t.len()..]);
                changed = true;
                continue;
            }
        }
        out.push_str(line);
    }
    if !seen {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[Plugins]\nLoadReshade=true\n");
        changed = true;
    }
    changed.then_some(out)
}

pub const REFRAMEWORK_ZIP: &str =
    "https://github.com/praydog/REFramework-nightly/releases/latest/download/REFramework.zip";

/// praydog's monolithic nightly: one `dinput8.dll` that detects the RE Engine
/// game at runtime (DMC5, RE2/3/4/7/8/9, MHRise, MHWilds, SF6, DD2, Pragmata...).
/// Only the DLL is extracted, as its release notes insist.
fn step_reframework(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if st.reframework {
        progress(100, "REFramework already present");
        return Ok(vec![]);
    }
    let d = st.game_dir();
    let zip_path = work.join("REFramework.zip");
    net::download(client, REFRAMEWORK_ZIP, &zip_path, "REFramework", progress)?;
    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("REFramework download is not a valid zip")?;
    let member = zip
        .file_names()
        .find(|n| net::file_name(n).eq_ignore_ascii_case(game::REFRAMEWORK_DLL))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("REFramework.zip has no {}", game::REFRAMEWORK_DLL))?;
    net::extract_member(&mut zip, &member, &d.join(game::REFRAMEWORK_DLL))?;
    fs::write(d.join(game::REFRAMEWORK_MARKER), b"")?;
    Ok(vec![game::REFRAMEWORK_DLL.to_owned()])
}

fn step_renodx(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    progress(0, "Looking up the RenoDX mod for this game");
    let m = renodx::lookup(client, &st.exe)?
        .ok_or_else(|| anyhow!("no RenoDX mod is published for this game"))?;
    renodx::install(client, &st.exe, &m, progress)
}

/// `with_renodx` adds the game's RenoDX HDR mod after the DLSS 5 add-on. On
/// the OptiScaler engine that needs ReShade too, loaded by OptiScaler as
/// `ReShade64.dll`. RE Engine games get REFramework first on either engine.
pub fn plan_with(st: &GameStatus, engine: Engine, with_renodx: bool) -> Vec<Step> {
    let mut v = if engine == Engine::Opti {
        // Only games with native DLSS: the NR pass reads the inputs the game
        // hands to DLSS. Callers gate on mode; return the plan regardless so
        // --check can show it.
        let mut v = vec![STEP_OPTI, STEP_DLSSNR_ONLY];
        if with_renodx {
            v.push(STEP_RESHADE_VIA_OPTI);
            v.push(STEP_RENODX);
        }
        v
    } else {
        let mut v = plan_reshade(st);
        if with_renodx {
            let at = v.len() - 1; // before ReShade config
            v.insert(at, STEP_RENODX);
        }
        v
    };
    if st.re_engine {
        v.insert(0, STEP_REFRAMEWORK);
    }
    v.push(STEP_GPU_PREF);
    v
}

fn plan_reshade(st: &GameStatus) -> Vec<Step> {
    match st.mode {
        game::Mode::Feeder => {
            let mut v = vec![STEP_RESHADE];
            if st.is32() {
                v.push(STEP_HOST_RESHADE);
            }
            v.extend([
                STEP_HEADERS,
                STEP_FEEDER,
                STEP_LUMENITE,
                STEP_DLSS5,
                STEP_CONFIG,
            ]);
            v
        }
        game::Mode::Native => {
            let mut v = vec![STEP_RESHADE];
            if st.feeder {
                v.push(STEP_FEEDER_CLEANUP);
            }
            v.push(STEP_DLSS5);
            if st.needs_bridge() {
                v.push(STEP_BRIDGE);
            }
            v.push(STEP_CONFIG);
            v
        }
    }
}

// ── release picking ────────────────────────────────────────────────

fn ver_key(tag: &str, prefix: &str) -> Vec<u64> {
    Regex::new(r"\d+")
        .unwrap()
        .find_iter(&tag[prefix.len()..])
        .filter_map(|m| m.as_str().parse().ok())
        .collect()
}

/// Newest rhi-repo release whose tag is `prefix` + digits; returns (tag, first asset URL).
pub fn pick_latest_asset(releases: &[Value], prefix: &str) -> Result<(String, String)> {
    let cands: Vec<(Vec<u64>, String, String)> = releases
        .iter()
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let rest = tag.strip_prefix(prefix)?;
            if !rest.chars().next()?.is_ascii_digit() {
                return None; // "dlss-" must not match "dlssg-"
            }
            let url = r
                .get("assets")?
                .as_array()?
                .first()?
                .get("browser_download_url")?
                .as_str()?;
            Some((ver_key(tag, prefix), tag.to_owned(), url.to_owned()))
        })
        .collect();
    if cands.is_empty() {
        bail!("no release with tag prefix '{prefix}' found");
    }
    Ok(best_tag(cands))
}

/// Newest by version; for the DLSS 5 model prefer ShortFuse's multi-generation
/// `.SF` builds over NVIDIA's RTX-50-only originals or single-generation ports.
fn best_tag(mut cands: Vec<(Vec<u64>, String, String)>) -> (String, String) {
    let any_sf = cands
        .iter()
        .any(|(_, t, _)| t.starts_with("dlssnr-") && t.contains(".SF"));
    if any_sf {
        cands.retain(|(_, t, _)| t.contains(".SF"));
    }
    cands.sort();
    let (_, tag, url) = cands.pop().unwrap();
    (tag, url)
}

/// rhi-repo lookup that never needs the API: HTML releases pages for the tag,
/// the expanded-assets fragment for the file.
pub fn rhi_latest(client: &Client, prefix: &str) -> Result<(String, String)> {
    if let Ok(releases) = net::get_json_github(client, RHI_RELEASES) {
        if let Some(arr) = releases.as_array() {
            if let Ok(r) = pick_latest_asset(arr, prefix) {
                return Ok(r);
            }
        }
    }
    let tags = net::github_release_tags_html(client, RHI_REPO, prefix, 6)?;
    let cands: Vec<(Vec<u64>, String, String)> = tags
        .into_iter()
        .filter(|t| {
            t[prefix.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .map(|t| (ver_key(&t, prefix), t, String::new()))
        .collect();
    if cands.is_empty() {
        bail!("no release with tag prefix '{prefix}' found on github.com/{RHI_REPO}/releases");
    }
    let (tag, _) = best_tag(cands);
    let url = net::github_asset_url_html(client, RHI_REPO, &tag, r#"[^"]+\.zip"#)?;
    Ok((tag, url))
}

// ── step 1: ReShade ────────────────────────────────────────────────

pub fn resolve_reshade_setup(client: &Client) -> Result<(String, String)> {
    let html = net::get_text(client, RESHADE_HOME)?;
    let re = Regex::new(r"/downloads/ReShade_Setup_([\d.]+)_Addon\.exe").unwrap();
    let m = re
        .captures(&html)
        .ok_or_else(|| anyhow!("ReShade add-on installer link not found on reshade.me"))?;
    Ok((m[1].to_owned(), format!("{RESHADE_HOME}{}", &m[0])))
}

pub fn install_reshade_from_setup(
    setup_exe: &Path,
    game_dir: &Path,
    bitness: u8,
    dest_name: &str,
) -> Result<Vec<String>> {
    let dll = if bitness == 64 {
        "ReShade64.dll"
    } else {
        "ReShade32.dll"
    };
    let f = fs::File::open(setup_exe)?;
    let mut zip = zip::ZipArchive::new(f).context("ReShade installer has no readable archive")?;
    net::extract_member(&mut zip, dll, &game_dir.join(dest_name))
        .with_context(|| format!("{} does not contain {dll}", setup_exe.display()))?;
    Ok(vec![dest_name.into()])
}

fn step_reshade(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let proxy = d.join(game::RESHADE_PROXY);
    if !st.reshade && proxy.is_file() {
        bail!(
            "{} exists but is not ReShade (DXVK, Special K, another injector?). Remove it first.",
            game::RESHADE_PROXY
        );
    }
    progress(0, "Looking up latest ReShade");
    let (ver, url) = resolve_reshade_setup(client)?;
    if st.reshade {
        // Only a copy this tool placed is refreshed; a user's own ReShade stays.
        match fs::read_to_string(d.join(game::RESHADE_MARKER)) {
            Ok(mine) if mine.trim() == ver => {
                return Ok(vec![format!("ReShade already current ({ver})")]);
            }
            Ok(_) => progress(0, &format!("ReShade {ver} is out, refreshing")),
            Err(_) => {
                return Ok(vec![
                    "ReShade present (not placed by this tool, left as is)".to_owned(),
                ]);
            }
        }
    }
    let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
    net::download(client, &url, &setup, "ReShade", progress)?;
    let out = install_reshade_from_setup(&setup, d, st.bitness, game::RESHADE_PROXY)?;
    fs::write(d.join(game::RESHADE_MARKER), ver.as_bytes())?;
    Ok(out)
}

// ── step 2: ReShade shader headers ────────────────────────────────

fn step_headers(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let shaders = st.game_dir().join("reshade-shaders").join("Shaders");
    let mut installed = Vec::new();
    for h in game::RESHADE_HEADERS {
        let dest = shaders.join(h);
        if dest.is_file() {
            continue;
        }
        net::download(
            client,
            &format!("{RESHADE_SHADERS_RAW}{h}"),
            &dest,
            h,
            progress,
        )?;
        installed.push(format!("reshade-shaders/Shaders/{h}"));
    }
    if installed.is_empty() {
        progress(100, "ReShade shader headers already present");
    }
    Ok(installed)
}

// ── step 3: DLSS5-Feeder ───────────────────────────────────────────

/// A release whose tag says beta or rc. Upstream does not flag all of them
/// as prereleases, so the name is what the install log goes by.
fn is_prerelease_tag(tag: &str) -> bool {
    let t = tag.to_ascii_lowercase();
    t.contains("beta") || t.contains("-rc") || t.contains("alpha")
}

fn step_feeder(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    // An installed Feeder used to be left alone forever (a 0.7.0 survived every
    // reinstall while 0.12.0 was out, #6). The zip is small: fetch it and
    // compare the add-on's size with what is on disk.
    progress(0, "Looking up latest DLSS5-Feeder");
    // Since 0.11 the project ships one zip per release instead of loose assets;
    // the file name carries the version, so the tag is read first.
    //
    // Whatever upstream marks as the latest release is what gets installed,
    // including a tag named "-beta": that project publishes builds it means
    // people to run with prerelease=false (v0.13.1-beta.1, v0.12.1-beta.2)
    // while flagging the ones it does not (v0.13.0-beta.1). Those carry
    // fixes the stable v0.12.0 lacks. The name is reported, so a beta is
    // never installed silently.
    let tag = match net::latest_tag(client, FEEDER_REPO) {
        Ok(t) => t,
        Err(_) => net::github_release_tags_html(client, FEEDER_REPO, "v", 1)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no DLSS5-Feeder release found"))?,
    };
    let tag = &tag;
    let note = if is_prerelease_tag(tag) {
        " (beta)"
    } else {
        ""
    };
    let url = net::github_asset_url_html(client, FEEDER_REPO, tag, r#"[^"]+\.zip"#)?;
    let zip_path = work.join("dlss5-feeder.zip");
    net::download(client, &url, &zip_path, "DLSS5-Feeder", progress)?;

    let d = st.game_dir();
    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("DLSS5-Feeder download is not a valid zip")?;
    let members: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let pick = |want: &str| -> Option<String> {
        members
            .iter()
            .find(|m| net::file_name(&m.replace('\\', "/")).eq_ignore_ascii_case(want))
            .cloned()
    };
    // 32-bit: the in-game half is addon32 and the 64-bit helper exe goes to
    // host64\; both must come from the same zip (helper protocol).
    let addon_name = if st.is32() {
        game::FEEDER_ADDON32
    } else {
        game::FEEDER_ADDON
    };
    let addon =
        pick(addon_name).ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {addon_name}"))?;
    let fx = pick(game::FEEDER_FX)
        .ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {}", game::FEEDER_FX))?;
    let host_member = st.is32().then(|| pick(game::HOST_EXE)).flatten();
    if st.is32() && host_member.is_none() {
        bail!("DLSS5-Feeder {tag} has no {}", game::HOST_EXE);
    }
    let host_current = match &host_member {
        Some(m) => same_size(&mut zip, m, &st.consumer_dir().join(game::HOST_EXE)),
        None => true,
    };
    if st.feeder && host_current && same_size(&mut zip, &addon, &d.join(addon_name)) {
        return Ok(vec![format!("DLSS5-Feeder already current ({tag}{note})")]);
    }
    net::extract_member(&mut zip, &addon, &d.join(addon_name))?;
    fs::write(d.join(game::FEEDER_MARKER), tag.as_bytes())?;
    let mut out = vec![format!("{addon_name} ({tag}{note})")];
    if let Some(m) = &host_member {
        let host = st.consumer_dir();
        fs::create_dir_all(&host)?;
        net::extract_member(&mut zip, m, &host.join(game::HOST_EXE))?;
        out.push(format!(
            "{}/{} ({tag}{note})",
            game::HOST_DIR,
            game::HOST_EXE
        ));
    }
    net::extract_member(
        &mut zip,
        &fx,
        &d.join("reshade-shaders")
            .join("Shaders")
            .join(game::FEEDER_FX),
    )?;
    out.push(format!("reshade-shaders/Shaders/{}", game::FEEDER_FX));
    Ok(out)
}

// ── step 4: LumeniteFX ─────────────────────────────────────────────

pub fn install_lumenite_from_zip(zip_path: &Path, game_dir: &Path) -> Result<Vec<String>> {
    let shaders = game_dir.join("reshade-shaders").join("Shaders");
    let textures = game_dir.join("reshade-shaders").join("Textures");
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("LumeniteFX download is not a valid zip")?;
    let fx = net::members_matching(
        &zip,
        &Regex::new(r"(?i)/Shaders/lumenite_[^/]+\.fx$").unwrap(),
    );
    let fxh = net::members_matching(
        &zip,
        &Regex::new(r"(?i)/Shaders/include/[^/]+\.fxh$").unwrap(),
    );
    let png = net::members_matching(
        &zip,
        &Regex::new(r"(?i)/Textures/lumenite_bluenoise256\.png$").unwrap(),
    );
    if fx.is_empty() || png.is_empty() {
        bail!("LumeniteFX archive layout changed; shaders or texture not found");
    }
    let mut installed = Vec::new();
    for (members, dir, rel) in [
        (&fx, shaders.clone(), "reshade-shaders/Shaders"),
        (
            &fxh,
            shaders.join("include"),
            "reshade-shaders/Shaders/include",
        ),
        (&png, textures, "reshade-shaders/Textures"),
    ] {
        for m in members {
            let name = net::file_name(m);
            net::extract_member(&mut zip, m, &dir.join(name))?;
            installed.push(format!("{rel}/{name}"));
        }
    }
    Ok(installed)
}

fn step_lumenite(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if st.lumenite {
        progress(100, "LumeniteFX already installed");
        return Ok(vec![]);
    }
    let z = work.join("LumeniteFX.zip");
    net::download(client, LUMENITE_ZIP, &z, "LumeniteFX", progress)?;
    install_lumenite_from_zip(&z, st.game_dir())
}

// ── step 5: DLSS 5 add-on + models ─────────────────────────────────

/// True when `dest` exists with the uncompressed size of `member`. Cheap
/// "is this the same build" check for files whose names carry no version.
pub fn same_size<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    member: &str,
    dest: &Path,
) -> bool {
    let local = fs::metadata(dest).map(|m| m.len()).ok();
    let remote = zip.by_name(member).ok().map(|f| f.size());
    local.is_some() && local == remote
}

pub fn install_single_from_zip(zip_path: &Path, member_name: &str, dest: &Path) -> Result<()> {
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f)
        .with_context(|| format!("{} is not a valid zip", zip_path.display()))?;
    let hit = zip
        .file_names()
        .find(|n| net::file_name(n).eq_ignore_ascii_case(member_name))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{} does not contain {member_name}", zip_path.display()))?;
    net::extract_member(&mut zip, &hit, dest)
}

fn step_dlss5(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    // A game with its own DLSS keeps its own nvngx_dlss.dll.
    let dlss_present = st.dlss || st.mode == game::Mode::Native;
    // Every piece is re-checked: the add-on by comparing its (small) zip, the
    // two NVIDIA DLLs by the release tag recorded when this tool placed them.
    // A DLL without a marker is the game's or the user's and is left alone.
    let plan = [
        ("renodx-dlss5-", game::DLSS5_ADDON, false, None),
        (
            "dlssnr-",
            game::DLSSNR_DLL,
            st.dlssnr,
            Some(game::DLSSNR_MARKER),
        ),
        (
            "dlss-",
            game::DLSS_DLL,
            dlss_present,
            Some(game::DLSS_MARKER),
        ),
    ];
    progress(0, "Looking up DLSS 5 add-on releases");
    let cdir = st.consumer_dir();
    fs::create_dir_all(&cdir)?;
    let mut installed = Vec::new();
    for (prefix, fname, present, marker) in plan {
        let (tag, url) = rhi_latest(client, prefix)?;
        if present {
            match marker.map(|m| fs::read_to_string(cdir.join(m))) {
                Some(Ok(mine)) if mine.trim() == tag => {
                    installed.push(format!("{fname} already current ({tag})"));
                    continue;
                }
                Some(Ok(_)) => progress(0, &format!("{fname}: {tag} is out, refreshing")),
                _ => {
                    installed.push(format!("{fname} present (not placed by this tool)"));
                    continue;
                }
            }
        }
        let z = work.join(format!("{tag}.zip"));
        net::download(client, &url, &z, fname, progress)?;
        let dest = cdir.join(fname);
        if fname == game::DLSS5_ADDON && st.dlss5_addon {
            let f = fs::File::open(&z)?;
            let mut zip =
                zip::ZipArchive::new(f).context("DLSS 5 add-on download is not a valid zip")?;
            let hit = zip
                .file_names()
                .find(|n| net::file_name(n).eq_ignore_ascii_case(fname))
                .map(str::to_owned);
            if hit.is_some_and(|h| same_size(&mut zip, &h, &dest)) {
                installed.push(format!("{fname} already current ({tag})"));
                continue;
            }
        }
        install_single_from_zip(&z, fname, &dest)?;
        if let Some(m) = marker {
            fs::write(cdir.join(m), tag.as_bytes())?;
        }
        let shown = if st.is32() {
            format!("{}/{fname} ({tag})", game::HOST_DIR)
        } else {
            format!("{fname} ({tag})")
        };
        installed.push(shown);
    }
    Ok(installed)
}

// ── opti engine: just the model DLL beside OptiScaler ───────────────

fn step_dlssnr_only(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    progress(0, "Looking up DLSS 5 model releases");
    let (tag, url) = rhi_latest(client, "dlssnr-")?;
    if st.dlssnr {
        match fs::read_to_string(st.game_dir().join(game::DLSSNR_MARKER)) {
            Ok(mine) if mine.trim() == tag => {
                return Ok(vec![format!(
                    "{} already current ({tag})",
                    game::DLSSNR_DLL
                )]);
            }
            Ok(_) => progress(
                0,
                &format!("{}: {tag} is out, refreshing", game::DLSSNR_DLL),
            ),
            Err(_) => {
                return Ok(vec![format!(
                    "{} present (not placed by this tool)",
                    game::DLSSNR_DLL
                )]);
            }
        }
    }
    let z = work.join(format!("{tag}.zip"));
    net::download(client, &url, &z, game::DLSSNR_DLL, progress)?;
    install_single_from_zip(&z, game::DLSSNR_DLL, &st.game_dir().join(game::DLSSNR_DLL))?;
    fs::write(st.game_dir().join(game::DLSSNR_MARKER), tag.as_bytes())?;
    Ok(vec![format!("{} ({tag})", game::DLSSNR_DLL)])
}

// ── native mode: a Feeder left over from an earlier install must go ─

fn step_feeder_cleanup(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let mut removed = Vec::new();
    for f in [
        d.join(game::FEEDER_ADDON),
        d.join("reshade-shaders")
            .join("Shaders")
            .join(game::FEEDER_FX),
    ] {
        if f.is_file() {
            fs::remove_file(&f)?;
            removed.push(
                f.strip_prefix(d)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    reshade_ini::remove_our_techniques(d)?;
    progress(
        100,
        "DLSS5-Feeder removed; the add-on hooks the game's own DLSS",
    );
    Ok(removed)
}

// ── step 5b: DX11 bridge (native-DLSS games rendering with D3D11) ──

fn step_bridge(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let dest = st.game_dir().join(game::BRIDGE_ADDON);
    // The bridge has no version tag in its file name and its releases fix
    // add-on-specific behaviour (1.4.0: the 2026-08-28 add-on build), so an
    // existing copy is refreshed whenever the published file differs in size.
    if st.bridge && dest.is_file() {
        let local = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        match net::remote_len(client, BRIDGE_DOWNLOAD) {
            Ok(Some(remote)) if remote != local => {
                progress(0, "dlss5-bridge changed upstream, refreshing");
            }
            Ok(_) => {
                return Ok(vec!["dlss5-bridge.addon64 already current".to_owned()]);
            }
            Err(_) => {
                return Ok(vec![
                    "dlss5-bridge.addon64 present (could not check for a newer one)".to_owned(),
                ]);
            }
        }
    } else {
        progress(0, "Fetching latest dlss5-bridge");
    }
    net::download(client, BRIDGE_DOWNLOAD, &dest, game::BRIDGE_ADDON, progress)?;
    Ok(vec![game::BRIDGE_ADDON.into()])
}

// ── step 6: config ─────────────────────────────────────────────────

fn step_config(_c: &Client, st: &GameStatus, _w: &Path, progress: Progress) -> Result<Vec<String>> {
    reshade_ini::write_reshade_ini(st.game_dir())?;
    reshade_ini::clear_disabled_addons(st.game_dir())?;
    if st.mode == game::Mode::Native {
        progress(100, "ReShade.ini written");
        return Ok(vec![game::RESHADE_INI.into()]);
    }
    reshade_ini::write_preset(st.game_dir())?;
    progress(100, "ReShade.ini + ReShadePreset.ini written");
    Ok(vec![game::RESHADE_INI.into(), game::RESHADE_PRESET.into()])
}

// ── step 7: which GPU Windows starts the process on ────────────

/// On a hybrid machine Windows may start the game (or the 32-bit helper) on the
/// iGPU, where NGX does not exist and `NVSDK_NGX_D3D12_Init` answers
/// `0xBAD00001`. That is what a reporter fixed by hand in Settings ▸ System ▸
/// Display ▸ Graphics (#25); this writes the same preference.
fn step_gpu_pref(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if !gpupref::hybrid() {
        progress(100, "one GPU vendor on this machine, nothing to set");
        return Ok(vec![]);
    }
    let mut targets = vec![st.exe.clone()];
    if st.is32() {
        targets.push(st.consumer_dir().join(game::HOST_EXE));
    }
    let mut out = Vec::new();
    for exe in targets.into_iter().filter(|p| p.is_file()) {
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match gpupref::set_high_performance(&exe) {
            Ok(true) => out.push(format!(
                "{name}: Windows GPU preference set to high performance"
            )),
            Ok(false) => out.push(format!("{name}: already set to the high-performance GPU")),
            Err(e) => out.push(format!("{name}: could not set the GPU preference ({e})")),
        }
    }
    progress(100, "GPU preference checked");
    Ok(out)
}

// ── driver ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Start,
    Done,
    Error,
}

pub fn run_all_with(
    exe: &Path,
    engine: Engine,
    with_renodx: bool,
    progress: Progress,
    step_cb: &(dyn Fn(usize, usize, &str, StepState, &str) + Sync),
) -> Result<Vec<(String, Vec<String>)>> {
    let mut st = game::inspect(exe)?;
    if !st.problems.is_empty() {
        bail!("{}", st.problems.join("\n"));
    }
    if engine == Engine::Opti && st.is32() {
        bail!("The OptiScaler engine is 64-bit only; a 32-bit game takes the Feeder path.");
    }
    if engine == Engine::Opti && st.mode != game::Mode::Feeder {
        // fine: native DLSS present
    } else if engine == Engine::Opti {
        bail!(
            "The OptiScaler engine needs a game with its own DLSS (its Neural Rendering pass \
             reads the inputs the game hands to DLSS). This game has none — use the ReShade engine."
        );
    }
    let client = net::client()?;
    let work = tempfile::Builder::new()
        .prefix("dlss5oneclick-")
        .tempdir()?;
    let steps = plan_with(&st, engine, with_renodx);
    let n = steps.len();
    let mut results = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        step_cb(i, n, step.name, StepState::Start, "");
        match (step.run)(&client, &st, work.path(), progress) {
            Ok(files) => {
                let detail = if files.is_empty() {
                    "already present".to_owned()
                } else {
                    files.join(", ")
                };
                step_cb(i, n, step.name, StepState::Done, &detail);
                results.push((step.name.to_owned(), files));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                step_cb(i, n, step.name, StepState::Error, &msg);
                return Err(anyhow!("{}: {msg}", step.name));
            }
        }
        st = game::inspect(exe)?;
    }
    Ok(results)
}

/// Remove everything this tool places except ReShade itself and nvngx_dlss.dll.
pub fn uninstall(exe: &Path) -> Result<Vec<String>> {
    let d = exe.parent().context("exe has no parent")?;
    let shaders = d.join("reshade-shaders").join("Shaders");
    let include = shaders.join("include");
    let mut targets: Vec<PathBuf> = vec![
        d.join(game::DLSS_MARKER),
        d.join(game::DLSSNR_MARKER),
        d.join(game::FEEDER_MARKER),
        d.join(game::FEEDER_ADDON),
        d.join(game::DLSS5_ADDON),
        d.join(game::DLSSNR_DLL),
        d.join(game::BRIDGE_ADDON),
        d.join("dlss5-dx11-bridge.addon64"),
        shaders.join(game::FEEDER_FX),
        d.join("reshade-shaders")
            .join("Textures")
            .join(game::LUMENITE_BLUENOISE),
    ];
    targets.extend(game::RESHADE_HEADERS.iter().map(|h| shaders.join(h)));
    for (dir, ext) in [(&shaders, "fx"), (&include, "fxh")] {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.starts_with("lumenite_") && name.ends_with(&format!(".{ext}")) {
                    targets.push(e.path());
                }
            }
        }
    }
    if d.join(game::DLSS_MARKER).is_file() {
        targets.push(d.join(game::DLSS_DLL));
    }
    // 32-bit layout: the in-game addon32 and everything in host64\.
    targets.push(d.join(game::FEEDER_ADDON32));
    let host = d.join(game::HOST_DIR);
    if host.is_dir() {
        for f in [
            game::HOST_EXE,
            game::DLSS5_ADDON,
            game::DLSSNR_DLL,
            game::DLSSNR_MARKER,
            game::DLSS_MARKER,
        ] {
            targets.push(host.join(f));
        }
        if host.join(game::DLSS_MARKER).is_file() {
            targets.push(host.join(game::DLSS_DLL));
        }
        if host.join(game::RESHADE_MARKER).is_file() {
            targets.push(host.join(game::RESHADE_PROXY));
            targets.push(host.join(game::RESHADE_MARKER));
        }
        for n in [
            "ReShade.ini",
            "ReShade.log",
            "dlss5-feed-host.log",
            "ReShadePreset.ini",
        ] {
            targets.push(host.join(n));
        }
    }
    if let Ok(name) = fs::read_to_string(d.join(game::RENODX_MANIFEST)) {
        let name = name.trim();
        if name.starts_with("renodx-") && !name.contains(['/', '\\']) {
            targets.push(d.join(name));
        }
        targets.push(d.join(game::RENODX_MANIFEST));
    }
    if d.join(game::REFRAMEWORK_MARKER).is_file() {
        targets.push(d.join(game::REFRAMEWORK_DLL));
        targets.push(d.join(game::REFRAMEWORK_MARKER));
    }
    let mut removed = Vec::new();
    uninstall_opti(d, &mut removed)?;
    for t in targets {
        if t.is_file() {
            fs::remove_file(&t)?;
            removed.push(
                t.strip_prefix(d)
                    .unwrap_or(&t)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    if include.is_dir() && fs::read_dir(&include)?.next().is_none() {
        fs::remove_dir(&include)?;
    }
    // The Windows GPU preference this tool wrote goes too, but only when it is
    // still exactly what was written (a user's own choice is left alone).
    for e in [
        exe.to_path_buf(),
        d.join(game::HOST_DIR).join(game::HOST_EXE),
    ] {
        if gpupref::clear_ours(&e).unwrap_or(false) {
            removed.push(format!(
                "Windows GPU preference for {}",
                e.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    let host = d.join(game::HOST_DIR);
    if host.is_dir() && fs::read_dir(&host)?.next().is_none() {
        fs::remove_dir(&host)?;
        removed.push(format!("{}/", game::HOST_DIR));
    }
    Ok(removed)
}

/// `uninstall`, then ReShade itself — but only when nothing foreign remains.
///
/// Refuses to touch ReShade when, after removing this tool's files, the game
/// still has other `.addon64`/`.addon32` files or other shaders in
/// `reshade-shaders` — that is somebody's own ReShade setup. `dxgi.dll` is
/// only deleted when it verifiably is a ReShade DLL. Returns
/// `(removed, kept_reason)`; `kept_reason` is `Some` when ReShade was left.
pub fn uninstall_all(exe: &Path) -> Result<(Vec<String>, Option<String>)> {
    let mut removed = uninstall(exe)?;
    let d = exe.parent().context("exe has no parent")?;

    let mut foreign: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".addon64") || n.ends_with(".addon32") {
                foreign.push(n);
            }
        }
    }
    let shaders_root = d.join("reshade-shaders");
    let mut walk = vec![shaders_root.clone()];
    while let Some(dir) = walk.pop() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk.push(p);
                } else {
                    foreign.push(
                        p.strip_prefix(d)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
    }
    if !foreign.is_empty() {
        foreign.sort();
        foreign.truncate(6);
        return Ok((
            removed,
            Some(format!(
                "ReShade left in place: the game still has files this tool did not install ({})",
                foreign.join(", ")
            )),
        ));
    }

    let mut rm = |p: PathBuf| -> Result<()> {
        if p.is_file() {
            fs::remove_file(&p)?;
            removed.push(
                p.strip_prefix(d)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        Ok(())
    };
    let proxy = d.join(game::RESHADE_PROXY);
    if game::is_reshade_dll(&proxy) {
        rm(proxy)?;
    }
    rm(d.join(game::RESHADE_MARKER))?;
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            let reshade_file = (n.starts_with("reshade")
                && (n.ends_with(".ini") || n.ends_with(".log")))
                || n.starts_with("reshadepreset")
                || n.starts_with("dlss5-feed.");
            if reshade_file {
                rm(e.path())?;
            }
        }
    }
    if shaders_root.is_dir() {
        fs::remove_dir_all(&shaders_root)?;
        removed.push("reshade-shaders/".into());
    }
    Ok((removed, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::testutil::*;
    use serde_json::json;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn rhi_releases() -> Vec<Value> {
        ["streamline-2.13.0.0", "renodx-dlss5-4.55", "renodx-dlss5-4.5", "renodx-dlss5-3.3.4",
         "dlssnr-310.8.SF-v2", "dlssnr-310.8.SF", "dlssg-310.8.0", "dlssd-310.7.129",
         "dlss-310.8.0", "dlss-310.7.129", "DLSS-Enabler-4.9.0.7"]
            .iter()
            .map(|t| json!({"tag_name": t, "assets": [{"browser_download_url": format!("https://x/{t}.zip")}]}))
            .collect()
    }

    #[test]
    fn dlssnr_prefers_multi_generation_sf_build() {
        let r: Vec<Value> = ["dlssnr-310.8.0", "dlssnr-310.8.0-RTX40", "dlssnr-310.8.SF", "dlssnr-310.8.SF-v2", "dlssnr-310.9.0"]
            .iter()
            .map(|t| json!({"tag_name": t, "assets": [{"browser_download_url": format!("https://x/{t}.zip")}]}))
            .collect();
        assert_eq!(
            pick_latest_asset(&r, "dlssnr-").unwrap().0,
            "dlssnr-310.8.SF-v2"
        );
        assert!(pick_latest_asset(&r, "renodx-dlss5-").is_err());
    }

    #[test]
    fn latest_asset_versions_and_prefix_isolation() {
        let r = rhi_releases();
        assert_eq!(
            pick_latest_asset(&r, "renodx-dlss5-").unwrap().0,
            "renodx-dlss5-4.55"
        );
        assert_eq!(
            pick_latest_asset(&r, "dlssnr-").unwrap().0,
            "dlssnr-310.8.SF-v2"
        );
        assert_eq!(pick_latest_asset(&r, "dlss-").unwrap().0, "dlss-310.8.0");
        assert!(pick_latest_asset(&r, "nothing-").is_err());
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])], prefix: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(prefix).unwrap();
        let mut w = zip::ZipWriter::new(f);
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn reshade_from_setup_exe_with_prepended_stub() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let setup = t.path().join("ReShade_Setup_6.8.0_Addon.exe");
        let mut dll = b"MZ".to_vec();
        dll.extend(std::iter::repeat_n(0u8, 1 << 20));
        dll.extend_from_slice(b"ReShade");
        write_zip(
            &setup,
            &[("ReShade64.dll", &dll), ("ReShade32.dll", b"32")],
            &[b'M', b'Z', 0, 0, 0, 0, 0, 0],
        );
        assert_eq!(
            install_reshade_from_setup(&setup, t.path(), 64, game::RESHADE_PROXY).unwrap(),
            vec!["dxgi.dll"]
        );
        assert!(game::inspect(&exe).unwrap().reshade);
    }

    #[test]
    fn lumenite_zip_places_shaders_includes_texture_and_ignores_slip() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let z = t.path().join("LumeniteFX.zip");
        write_zip(
            &z,
            &[
                ("LumeniteFX-mainline/README.md", b"x"),
                (
                    "LumeniteFX-mainline/Shaders/lumenite_Kernel.fx",
                    b"technique Lumenite_Kernel {}",
                ),
                ("LumeniteFX-mainline/Shaders/lumenite_TRAA.fx", b"t"),
                (
                    "LumeniteFX-mainline/Shaders/include/lumenite_Helpers.fxh",
                    b"h",
                ),
                (
                    "LumeniteFX-mainline/Textures/lumenite_bluenoise256.png",
                    b"png",
                ),
                ("../evil.fx", b"zip-slip"),
            ],
            &[],
        );
        let installed = install_lumenite_from_zip(&z, t.path()).unwrap();
        assert_eq!(installed.len(), 4);
        assert!(t
            .path()
            .join("reshade-shaders/Shaders/lumenite_Kernel.fx")
            .is_file());
        assert!(t
            .path()
            .join("reshade-shaders/Shaders/include/lumenite_Helpers.fxh")
            .is_file());
        assert!(t
            .path()
            .join("reshade-shaders/Textures/lumenite_bluenoise256.png")
            .is_file());
        assert!(!t.path().parent().unwrap().join("evil.fx").exists());
        assert!(game::inspect(&exe).unwrap().lumenite);

        let bad = t.path().join("bad.zip");
        write_zip(&bad, &[("whatever.txt", b"x")], &[]);
        assert!(install_lumenite_from_zip(&bad, t.path()).is_err());
    }

    #[test]
    fn single_from_zip_and_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let z = t.path().join("renodx-dlss5-4.55.zip");
        write_zip(&z, &[("renodx-dlss5.addon64", b"addon")], &[]);
        install_single_from_zip(
            &z,
            "renodx-dlss5.addon64",
            &t.path().join("renodx-dlss5.addon64"),
        )
        .unwrap();
        assert!(game::inspect(&exe).unwrap().dlss5_addon);
        fs::write(t.path().join(game::DLSS_DLL), b"keep").unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&"renodx-dlss5.addon64".to_string()));
        assert!(t.path().join(game::DLSS_DLL).is_file());
    }

    #[test]
    fn thirty_two_bit_plan_status_and_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game32.exe"), game::PE_X86);
        let st = game::inspect(&exe).unwrap();
        assert!(st.is32());
        assert_eq!(st.mode, game::Mode::Feeder);
        assert!(st.problems.is_empty(), "{:?}", st.problems);
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[0], "ReShade (add-on build)");
        assert_eq!(names[1], "64-bit ReShade for the host64 helper");
        assert_eq!(names.len(), 8); // + the GPU-preference step
                                    // Lay the 32-bit result out by hand and check status + removal.
        let d = t.path();
        let host = d.join(game::HOST_DIR);
        fs::create_dir_all(d.join("reshade-shaders").join("Shaders")).unwrap();
        fs::create_dir_all(&host).unwrap();
        fs::write(d.join(game::FEEDER_ADDON32), b"a32").unwrap();
        fs::write(
            d.join("reshade-shaders")
                .join("Shaders")
                .join(game::FEEDER_FX),
            b"fx",
        )
        .unwrap();
        fs::write(host.join(game::HOST_EXE), b"host").unwrap();
        fs::write(host.join(game::DLSS5_ADDON), b"addon").unwrap();
        fs::write(host.join(game::DLSSNR_DLL), b"nr").unwrap();
        fs::write(host.join(game::DLSS_DLL), b"dlss").unwrap();
        fs::write(host.join(game::DLSS_MARKER), b"dlss-1").unwrap();
        make_reshade_dll(&host.join(game::RESHADE_PROXY));
        fs::write(host.join(game::RESHADE_MARKER), b"6.8.0").unwrap();
        let st = game::inspect(&exe).unwrap();
        assert!(
            st.feeder && st.dlss5_addon && st.dlssnr && st.dlss && st.host_exe && st.host_reshade
        );
        let removed = uninstall(&exe).unwrap();
        assert!(removed.iter().any(|r| r.contains(game::HOST_EXE)));
        assert!(!host.exists(), "host64 folder should be gone: {removed:?}");
        assert!(!d.join(game::FEEDER_ADDON32).exists());
    }

    #[test]
    fn plan_adds_reframework_first_and_renodx_before_config() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("re4.exe"), game::PE_X64);
        fs::write(t.path().join(game::RE_ENGINE_PAK), b"pak").unwrap();
        let mut st = game::inspect(&exe).unwrap();
        assert!(st.re_engine && !st.reframework);
        st.mode = game::Mode::Native;
        st.api = game::Api::Dx12;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, true)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "REFramework (RE Engine needs it before ReShade)",
                "ReShade (add-on build)",
                "DLSS 5 add-on + models",
                "RenoDX HDR mod for this game",
                "ReShade config",
                "GPU preference"
            ]
        );
        let names: Vec<&str> = plan_with(&st, Engine::Opti, true)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "REFramework (RE Engine needs it before ReShade)",
                "OptiScaler + DLSS Neural Rendering",
                "DLSS 5 model (nvngx_dlssnr.dll)",
                "ReShade loaded by OptiScaler (ReShade64.dll)",
                "RenoDX HDR mod for this game",
                "GPU preference"
            ]
        );
    }

    #[test]
    fn set_load_reshade_rewrites_or_appends() {
        let ini = "[Plugins]\r\n; doc\r\nLoadReshade=auto\r\nOther=1\r\n";
        assert_eq!(
            set_load_reshade(ini).unwrap(),
            "[Plugins]\r\n; doc\r\nLoadReshade=true\r\nOther=1\r\n"
        );
        assert!(set_load_reshade("LoadReshade=true\n").is_none());
        assert_eq!(
            set_load_reshade("[Upscalers]\nDx12Upscaler=auto\n").unwrap(),
            "[Upscalers]\nDx12Upscaler=auto\n\n[Plugins]\nLoadReshade=true\n"
        );
    }

    #[test]
    fn uninstall_removes_recorded_renodx_mod_and_reframework_only() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        fs::write(t.path().join("renodx-cp2077.addon64"), b"ours").unwrap();
        fs::write(t.path().join("renodx-ff7rebirth.addon64"), b"theirs").unwrap();
        fs::write(
            t.path().join(game::RENODX_MANIFEST),
            "renodx-cp2077.addon64\n",
        )
        .unwrap();
        fs::write(t.path().join(game::REFRAMEWORK_DLL), b"ref").unwrap();
        let st = game::inspect(&exe).unwrap();
        assert_eq!(st.renodx_mod.as_deref(), Some("renodx-cp2077.addon64"));
        assert_eq!(
            st.foreign_renodx,
            vec!["renodx-ff7rebirth.addon64".to_string()]
        );
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&"renodx-cp2077.addon64".to_string()));
        assert!(t.path().join("renodx-ff7rebirth.addon64").is_file());
        // dinput8.dll without our marker is somebody else's REFramework: kept.
        assert!(t.path().join(game::REFRAMEWORK_DLL).is_file());
        fs::write(t.path().join(game::REFRAMEWORK_MARKER), b"").unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&game::REFRAMEWORK_DLL.to_string()));
    }

    /// Upstream publishes some "-beta" tags with prerelease=false, so the tag
    /// name is what decides whether the install log says beta.
    /// Only a component this tool recorded can be reported as out of date;
    /// a user's own ReShade has no marker and must stay invisible.
    #[test]
    fn stale_components_reports_only_what_we_placed() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let latest = Latest {
            reshade: Some("6.8.0".into()),
            feeder: Some("v0.13.1-beta.1".into()),
            opti: Some("v0.2.0-dlssnr".into()),
            dlss: Some("dlss-310.9.0".into()),
            dlssnr: Some("dlssnr-310.8.SF-v2".into()),
        };
        assert!(stale_components(d, &latest).is_empty());

        fs::write(d.join(game::FEEDER_MARKER), "v0.12.0").unwrap();
        fs::write(d.join(game::DLSS_MARKER), "dlss-310.9.0").unwrap();
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.1.2-dlssnr\ndxgi.dll\n",
        )
        .unwrap();
        let stale = stale_components(d, &latest);
        assert_eq!(
            stale,
            vec![
                "DLSS5-Feeder v0.12.0 → v0.13.1-beta.1".to_string(),
                "OptiScaler v0.1.2-dlssnr → v0.2.0-dlssnr".to_string(),
            ]
        );

        // A manifest from before the tag was recorded cannot be compared, and
        // saying nothing would leave a stale install looking current.
        fs::write(d.join(game::OPTI_MANIFEST), "dxgi.dll\nOptiScaler.ini\n").unwrap();
        assert!(stale_components(d, &latest)
            .iter()
            .any(|s| s == "OptiScaler unknown version → v0.2.0-dlssnr"));
    }

    /// The manifest carries the tag on a comment line, and older manifests
    /// (written before that) must read as "unknown" rather than as a path.
    #[test]
    fn manifest_tag_is_read_from_the_header() {
        let m = "# tag v0.2.0-dlssnr\nOptiScaler.dll\ndxgi.dll\n";
        assert_eq!(manifest_tag(m).as_deref(), Some("v0.2.0-dlssnr"));
        assert_eq!(manifest_tag("OptiScaler.dll\ndxgi.dll\n"), None);
    }

    #[test]
    fn prerelease_tags_are_named_by_their_tag() {
        assert!(is_prerelease_tag("v0.13.1-beta.1"));
        assert!(is_prerelease_tag("v0.12.1-beta.2"));
        assert!(is_prerelease_tag("v1.0.0-rc.1"));
        assert!(!is_prerelease_tag("v0.12.0"));
        assert!(!is_prerelease_tag("v1.4.8"));
    }

    #[test]
    fn plan_follows_mode_and_api() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let mut st = game::inspect(&exe).unwrap();
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names.len(), 7); // + the GPU-preference step
        assert_eq!(names[2], "DLSS5-Feeder");
        st.mode = game::Mode::Native;
        st.api = game::Api::Dx12;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "ReShade (add-on build)",
                "DLSS 5 add-on + models",
                "ReShade config",
                "GPU preference"
            ]
        );
        st.api = game::Api::Dx11;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[2], "DLSS 5 DX11 bridge");
    }

    #[test]
    fn uninstall_all_removes_reshade_only_when_nothing_foreign_remains() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        let sh = d.join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&sh).unwrap();
        fs::write(d.join(game::FEEDER_ADDON), b"x").unwrap();
        fs::write(sh.join(game::FEEDER_FX), b"x").unwrap();
        fs::write(sh.join("ReShade.fxh"), b"x").unwrap();
        fs::write(d.join("ReShade.ini"), b"x").unwrap();
        fs::write(d.join("ReShadePreset.ini"), b"x").unwrap();
        fs::write(d.join("dlss5-feed.cfg"), b"x").unwrap();
        // a foreign shader blocks ReShade removal
        fs::write(sh.join("Clarity.fx"), b"user shader").unwrap();
        let (_removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_some());
        assert!(d.join("dxgi.dll").is_file());
        assert!(!d.join(game::FEEDER_ADDON).is_file());
        // without it, everything goes
        fs::remove_file(sh.join("Clarity.fx")).unwrap();
        let (removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_none(), "{kept:?}");
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join("ReShade.ini").exists());
        assert!(!d.join("dlss5-feed.cfg").exists());
        assert!(!d.join("reshade-shaders").exists());
    }

    #[test]
    fn uninstall_all_keeps_foreign_addons() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        fs::write(d.join("someones-mod.addon64"), b"x").unwrap();
        let (_removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_some());
        assert!(d.join("dxgi.dll").is_file());
    }

    #[test]
    fn opti_plan_and_engine_gate() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let st = game::inspect(&exe).unwrap();
        let names: Vec<&str> = plan_with(&st, Engine::Opti, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "OptiScaler + DLSS Neural Rendering",
                "DLSS 5 model (nvngx_dlssnr.dll)",
                "GPU preference"
            ]
        );
        // Feeder-mode game + Opti engine is refused before any network
        let err =
            run_all_with(&exe, Engine::Opti, false, &|_, _| {}, &|_, _, _, _, _| {}).unwrap_err();
        assert!(err.to_string().contains("own DLSS"));
    }

    #[test]
    fn uninstall_removes_opti_manifest_files() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        fs::create_dir_all(d.join("OptiScaler")).unwrap();
        fs::write(d.join("dxgi.dll"), b"opti").unwrap();
        fs::write(d.join("OptiScaler.ini"), b"ini").unwrap();
        fs::write(d.join("OptiScaler").join("libxess.dll"), b"x").unwrap();
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "dxgi.dll\nOptiScaler.ini\nOptiScaler/libxess.dll",
        )
        .unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join("OptiScaler").exists());
        assert!(!d.join(game::OPTI_MANIFEST).exists());
    }

    #[test]
    fn run_all_refuses_opti_on_32bit_before_network() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let err =
            run_all_with(&exe, Engine::Opti, false, &|_, _| {}, &|_, _, _, _, _| {}).unwrap_err();
        assert!(err.to_string().contains("64-bit only"));
    }
}
