//! The install steps, in the order the DLSS5-Feeder README lists them.
//!
//! Sources (verified 2026-08-31):
//! 0. dgVoodoo 2.87.3 — only when the game is Direct3D 9 and dgVoodoo is not
//!    already in the game folder. Downloaded from the official GitHub release
//!    (not bundled); extracts `MS/{x86|x64}/D3D9.dll` by exe bitness → `d3d9.dll`
//!    + smart-merged conf (force OutputAPI, floor VRAM, preserve the rest).
//! 1. ReShade add-on build — https://reshade.me links `/downloads/ReShade_Setup_<ver>_Addon.exe`;
//!    that exe has an appended ZIP with ReShade64.dll / ReShade32.dll. Dropped as dxgi.dll.
//! 2. ReShade shader headers — raw.githubusercontent.com/crosire/reshade-shaders/slim/Shaders/
//!    {ReShade.fxh, ReShadeUI.fxh, DrawText.fxh}; the setup exe only carries the DLLs.
//! 3. DLSS5-Feeder — jlrouzies-fr/DLSS5-Feeder latest release zip only
//!    (`dlss5-feed.addon64` + `DLSS5_Feed.fx`; `feed-vk-layer.zip` is Vulkan-only, unused).
//!    No local overwrite of Feeder binaries or shaders — official release assets only.
//! 4. LumeniteFX — umar-afzaal/LumeniteFX branch `mainline` (no releases):
//!    Shaders/lumenite_*.fx, Shaders/include/*.fxh, Textures/lumenite_bluenoise256.png.
//! 5. DLSS 5 add-on — RankFTW/rhi-repo releases: `renodx-dlss5-*` (renodx-dlss5.addon64),
//!    `dlssnr-*` (nvngx_dlssnr.dll), `dlss-*` (nvngx_dlss.dll; not dlssg-/dlssd-).
//! 6. ReShade.ini + ReShadePreset.ini: DLSS5_MV_PROVIDER=3, Lumenite_Kernel above DLSS5_Feed.
//!    Optional LUMENITE: TRAA stays user-controlled; we soft-patch UI protect + preset defaults.

use crate::game::{self, GameStatus};
use crate::gpupref;
use crate::net::{self, Progress};
use crate::quality_preset::{self, QualityChoice, QualityOverrides, ResolvedQuality};
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

/// Official dgVoodoo 2.87.3 release zip (not bundled — downloaded into the game
/// folder at Install time). License allows shipping individual DLLs with a
/// game; forbids bundling inside launchers for general multi-app use.
pub const DGVOODOO_TAG: &str = "v2.87.3";
pub const DGVOODOO_ZIP: &str =
    "https://github.com/dege-diosg/dgVoodoo2/releases/download/v2.87.3/dgVoodoo2_87_3.zip";
/// Zip members for D3D9 (32-bit Gothic-class vs rare 64-bit DX9).
const DGVOODOO_D3D9_MEMBER_X86: &str = "MS/x86/D3D9.dll";
const DGVOODOO_D3D9_MEMBER_X64: &str = "MS/x64/D3D9.dll";

/// Minimum emulated VRAM (MB). Stock dgVoodoo is 256 — too low for Gothic 3 VH @ 1080p.
const DGVOODOO_VRAM_FLOOR: u32 = 4096;
/// Feeder/ReShade need D3D11; never leave `bestavailable` (may pick D3D12).
const DGVOODOO_OUTPUT_API: &str = "d3d11_fl11_0";

/// Full template used only when no `dgVoodoo.conf` exists yet.
const DGVOODOO_CONF_TEMPLATE: &str = "\
; Written by DLSS5oneclick — official dgVoodoo 2.87.3 (DX9 → D3D11 for ReShade dxgi.dll)
; https://github.com/dege-diosg/dgVoodoo2/releases/tag/v2.87.3
[General]
OutputAPI = d3d11_fl11_0
Adapters = all
FullScreenOutput = default
ScalingMode = unspecified
[DirectX]
VideoCard = geforce_9800_gt
VRAM = 4096
Filtering = appdriven
Mipmapping = appdriven
Resolution = unforced
Antialiasing = appdriven
AppControlledScreenMode = true
ForceVerticalSync = false
dgVoodooWatermark = false
FastVideoMemoryAccess = false
[DirectXExt]
RTTexturesForceScaleAndMSAA = false
";

/// Marker embedded in the Lumenite TRAA UI-protect patch (idempotent).
const TRAA_UI_MARKER: &str = "DLSS5_TRAA_UI_PROTECT";
const TRAA_FX: &str = "lumenite_TRAA.fx";

/// Soft-patch installed `lumenite_TRAA.fx`: Geometric DLAA by default + skip temporal
/// blend where `DLSS5_Mask` / HUD-like luma edges without depth structure say so.
/// Leaves TRAA enabled/disabled as the user set it; only improves UI/text when on.
fn apply_traa_ui_patch(game_dir: &Path) -> Result<Option<String>> {
    let dest = game_dir
        .join("reshade-shaders")
        .join("Shaders")
        .join(TRAA_FX);
    if !dest.is_file() {
        return Ok(None);
    }
    let mut text =
        fs::read_to_string(&dest).with_context(|| format!("reading {}", dest.display()))?;
    if text.contains(TRAA_UI_MARKER) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect, already applied)"
        )));
    }

    // Default Edge Detection → Geometric (stock tooltip already says it ignores flat UI).
    let edge_anchor = "\"Geometric: silhouettes only, ignores flat UI.\";\n    > = 0;";
    if !text.contains(edge_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: EDGE_MODE layout changed)"
        )));
    }
    text = text.replace(
        edge_anchor,
        "\"Geometric: silhouettes only, ignores flat UI.\";\n    > = 1;",
    );

    let uniforms = r#"
// DLSS5_TRAA_UI_PROTECT -- favour current frame on HUD/text (DLSS5oneclick)
uniform bool UI_PROTECT <
    ui_label = "Protect UI / text (skip temporal)";
    ui_tooltip = "Lowers temporal blend where DLSS5_Mask distrusts motion, and where\n"
                 "sharp luma edges lack geometric depth/normal structure (typical HUD/text).\n"
                 "Needs Kernel above + DLSS 5 Feed above this effect for the bias mask.";
> = true;

uniform float UI_PROTECT_STRENGTH <
    ui_type = "drag";
    ui_min = 0.0; ui_max = 1.0; ui_step = 0.05;
    ui_label = "UI protect strength";
> = 1.0;

"#;
    let imports_anchor = "/*--------------.\n| :: IMPORTS :: |\n'--------------*/";
    if !text.contains(imports_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: IMPORTS layout changed)"
        )));
    }
    text = text.replace(imports_anchor, &format!("{uniforms}{imports_anchor}"));

    let mask_tex = r#"
// DLSS5_TRAA_UI_PROTECT -- same pooled mask Feed writes (bias-current-colour)
texture DLSS5_Mask { Width = BUFFER_WIDTH; Height = BUFFER_HEIGHT; Format = R8; };
sampler sDLSS5_Mask { Texture = DLSS5_Mask; MinFilter = POINT; MagFilter = POINT; MipFilter = POINT; };

"#;
    let ns_anchor = "namespace LumeniteTRAA {";
    if !text.contains(ns_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: namespace layout changed)"
        )));
    }
    text = text.replace(ns_anchor, &format!("{mask_tex}{ns_anchor}"));

    let conf_anchor = "    confidence = saturate(confidence + 0.11 * 4.0 * confidence * (1.0 - confidence));\n\n    float2 historyUV = texcoord + flow;";
    let conf_patch = r#"    confidence = saturate(confidence + 0.11 * 4.0 * confidence * (1.0 - confidence));

    // DLSS5_TRAA_UI_PROTECT
    if (UI_PROTECT)
    {
        float distrust = tex2Dlod(sDLSS5_Mask, float4(texcoord, 0.0, 0.0)).x;
        float4 nPack = tex2Dlod(Kernel::sNormals, float4(texcoord, 0.0, 0.0));
        float2 px = BUFFER_PIXEL_SIZE;
        float dL = tex2Dlod(Kernel::sNormals, float4(texcoord - float2(px.x, 0.0), 0.0, 0.0)).a;
        float dR = tex2Dlod(Kernel::sNormals, float4(texcoord + float2(px.x, 0.0), 0.0, 0.0)).a;
        float dT = tex2Dlod(Kernel::sNormals, float4(texcoord - float2(0.0, px.y), 0.0, 0.0)).a;
        float dB = tex2Dlod(Kernel::sNormals, float4(texcoord + float2(0.0, px.y), 0.0, 0.0)).a;
        float depthEdge = abs(dL - dR) + abs(dT - dB);
        float nEdge = length(nPack.xyz - tex2Dlod(Kernel::sNormals, float4(texcoord + float2(px.x, 0.0), 0.0, 0.0)).xyz);
        float lumaEdge = abs(GetLuminance(samples[3]) - GetLuminance(samples[5]))
                       + abs(GetLuminance(samples[1]) - GetLuminance(samples[7]));
        // Sharp text/HUD edges without geometric structure
        float uiHint = saturate(lumaEdge * 6.0) * (1.0 - saturate(depthEdge * 40.0 + nEdge * 4.0));
        // Screen-space UI often gets camera/scene flow while depth stays flat
        float mvPx = length(flow * float2(BUFFER_WIDTH, BUFFER_HEIGHT));
        float badFlow = saturate(mvPx * 0.25) * (1.0 - saturate(depthEdge * 40.0));
        float skip = saturate(max(max(distrust, uiHint), badFlow) * UI_PROTECT_STRENGTH);
        confidence *= (1.0 - skip);
    }

    float2 historyUV = texcoord + flow;"#;
    if !text.contains(conf_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: PS_TRAA layout changed)"
        )));
    }
    text = text.replace(conf_anchor, conf_patch);

    text = text.replace(
        "ui_tooltip = \"Temporal Reprojection Anti-Aliasing.\";",
        "ui_tooltip = \"Temporal Reprojection Anti-Aliasing.\\n\\n\
Place BELOW DLSS 5 Feed. Edge Detection=Geometric + Protect UI/text reduce HUD smear.\\n\
Uses DLSS5_Mask from Feed when present (DLSS5oneclick UI protect patch).\";",
    );

    fs::write(&dest, text).with_context(|| format!("writing patched {}", dest.display()))?;
    Ok(Some(format!(
        "reshade-shaders/Shaders/{TRAA_FX} (UI protect patch)"
    )))
}

const VULKAN_SETUP_TXT: &str = "\
DLSS5oneclick — Vulkan Feeder kit (manual finish)
================================================
This tool does NOT register ReShade as a Vulkan layer (that is why full
Install is refused). Files copied here still need ReShade's own setup.

1. Run ReShade Setup → select this game exe → choose Vulkan → Addon support.
2. In ReShade.ini next to the exe, under [ADDON]:
     AddonPath=<this folder>
3. Ensure dlss5-feed.addon64 and reshade-shaders/Shaders/DLSS5_Feed.fx are here
   (already copied by «Copy Vulkan Feeder kit»).
4. Also place a neural consumer (renodx-dlss5.addon64 + nvngx_dlssnr.dll) as for
   a 64-bit D3D game, or use Deep Fried Chicken per Feeder docs.
5. If dlss5-feed.log reports missing interop entry points, start the game via
   run-with-feed-layer.bat from the DLSS5-Feeder repo layer/ folder.

Do not expect dxgi.dll from this tool to load under Vulkan.
";

/// Drop Feeder addon + FX + setup note for manual Vulkan ReShade (no layer install).
/// Fetches the official DLSS5-Feeder release zip — never a bundled/modified add-on.
pub fn copy_vulkan_feeder_kit(game_dir: &Path) -> Result<Vec<String>> {
    let client = net::client()?;
    let tag = match net::latest_tag(&client, FEEDER_REPO) {
        Ok(t) => t,
        Err(_) => net::github_release_tags_html(&client, FEEDER_REPO, "v", 1)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no DLSS5-Feeder release found"))?,
    };
    let url = net::github_asset_url_html(&client, FEEDER_REPO, &tag, r#"[^"]+\.zip"#)?;
    let work = tempfile::tempdir()?;
    let zip_path = work.path().join("dlss5-feeder.zip");
    net::download(&client, &url, &zip_path, "DLSS5-Feeder", &|_, _| {})?;
    copy_vulkan_feeder_kit_from_zip(&zip_path, game_dir, &tag)
}

/// Extract official Feeder addon + FX from a release zip (testable offline).
pub fn copy_vulkan_feeder_kit_from_zip(
    zip_path: &Path,
    game_dir: &Path,
    tag: &str,
) -> Result<Vec<String>> {
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("DLSS5-Feeder download is not a valid zip")?;
    let members: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let pick = |want: &str| -> Option<String> {
        members
            .iter()
            .find(|m| net::file_name(&m.replace('\\', "/")).eq_ignore_ascii_case(want))
            .cloned()
    };
    let addon = pick(game::FEEDER_ADDON)
        .ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {}", game::FEEDER_ADDON))?;
    let fx = pick(game::FEEDER_FX)
        .ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {}", game::FEEDER_FX))?;
    net::extract_member(&mut zip, &addon, &game_dir.join(game::FEEDER_ADDON))?;
    net::extract_member(
        &mut zip,
        &fx,
        &game_dir
            .join("reshade-shaders")
            .join("Shaders")
            .join(game::FEEDER_FX),
    )?;
    let note = game_dir.join("VULKAN-SETUP.txt");
    fs::write(&note, VULKAN_SETUP_TXT).with_context(|| format!("writing {}", note.display()))?;
    Ok(vec![
        format!("{} ({tag})", game::FEEDER_ADDON),
        format!("reshade-shaders/Shaders/{}", game::FEEDER_FX),
        "VULKAN-SETUP.txt".into(),
    ])
}

/// Which install engine carries the DLSS 5 pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// ReShade + RenoDX add-on (both game kinds; the default).
    #[default]
    ReShade,
    /// Dagherbou's OptiScaler fork with the built-in Neural Rendering pass.
    /// Games with native DLSS only (the pass reads the inputs the game hands to DLSS).
    Opti,
    /// ReShade + kibblerz's standalone AIO add-on: neural rendering, super
    /// resolution and frame generation from one add-on, in games with no DLSS
    /// of their own. 64-bit games only here.
    Aio,
}

const STEP_AIO: Step = Step {
    name: "DLSS5 ReShade AIO (standalone add-on)",
    run: step_aio,
};
const STEP_AIO_RUNTIME: Step = Step {
    name: "NVIDIA DLSS + frame-generation runtimes",
    run: step_aio_runtime,
};
const STEP_AIO_CONFIG: Step = Step {
    name: "ReShade config",
    run: step_aio_config,
};
const STEP_AIO_CLEANUP: Step = Step {
    name: "Remove the standalone AIO add-on (another consumer replaces it)",
    run: step_aio_cleanup,
};
pub const AIO_REPO: &str = "kibblerz/DLSS5-Reshade-AIO";

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
    /// wilsjo2's pre-SR fork numbers its releases on its own, so its newest tag
    /// has to be carried separately from the stable build's.
    pub opti_presr: Option<String>,
    pub dlss: Option<String>,
    pub dlssnr: Option<String>,
    pub aio: Option<String>,
}

impl Latest {
    pub fn fetch(client: &Client) -> Self {
        Latest {
            reshade: resolve_reshade_setup(client).ok().map(|(v, _)| v),
            feeder: net::latest_tag(client, FEEDER_REPO).ok(),
            opti: net::latest_tag(client, OPTI_REPO).ok(),
            opti_presr: net::latest_tag(client, OPTI_PRESR_REPO).ok(),
            // Same source order as the install: NVIDIA's tag, else the mirror's.
            dlss: nvidia_dll(client, game::DLSS_DLL)
                .map(|(t, _)| t)
                .or_else(|| rhi_latest(client, "dlss-").ok().map(|(t, _)| t)),
            dlssnr: rhi_latest(client, "dlssnr-").ok().map(|(t, _)| t),
            aio: net::latest_tag(client, AIO_REPO).ok(),
        }
    }
}

/// Files that must exist after a successful Feeder/Native install.
/// Used so the UI never says "Everything is in place" on a partial copy.
pub fn missing_install_files(st: &GameStatus) -> Vec<String> {
    let mut missing = Vec::new();
    if st.aio && !st.opti {
        if !st.reshade {
            missing.push(format!("{} (ReShade)", game::RESHADE_PROXY));
        }
        if !st.dlssnr {
            missing.push(game::DLSSNR_DLL.into());
        }
        if !st.dlss {
            missing.push(game::DLSS_DLL.into());
        }
        return missing;
    }
    match st.mode {
        game::Mode::Feeder => {
            if !st.reshade {
                missing.push(format!("{} (ReShade)", game::RESHADE_PROXY));
            }
            if !st.headers {
                missing.push("reshade-shaders/Shaders headers (ReShade.fxh…)".into());
            }
            if !st.feeder {
                let addon = if st.is32() {
                    game::FEEDER_ADDON32
                } else {
                    game::FEEDER_ADDON
                };
                missing.push(format!("{addon} / {}", game::FEEDER_FX));
            }
            if !st.lumenite {
                missing.push("LumeniteFX shaders".into());
            }
            if !st.dlss5_addon && !st.upstream {
                missing.push("DLSS 5 neural consumer add-on".into());
            }
            if !st.dlssnr {
                missing.push(game::DLSSNR_DLL.into());
            }
            if !st.dlss {
                missing.push(game::DLSS_DLL.into());
            }
            if st.is32() {
                if !st.host_exe {
                    missing.push(format!("{}/{}", game::HOST_DIR, game::HOST_EXE));
                }
                if !st.host_reshade {
                    missing.push(format!("{}/{}", game::HOST_DIR, game::RESHADE_PROXY));
                }
            }
        }
        game::Mode::Native => {
            if st.opti {
                if !st.dlssnr {
                    missing.push(game::DLSSNR_DLL.into());
                }
            } else {
                if !st.reshade {
                    missing.push(format!("{} (ReShade)", game::RESHADE_PROXY));
                }
                if !(st.dlss5_addon || st.upstream) {
                    missing.push("DLSS 5 neural consumer add-on".into());
                }
                if !st.dlssnr {
                    missing.push(game::DLSSNR_DLL.into());
                }
                if st.needs_bridge() && !st.bridge {
                    missing.push("dx11 bridge add-on".into());
                }
            }
        }
    }
    missing
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
    if let Ok(m) = fs::read_to_string(dir.join(game::AIO_MANIFEST)) {
        check("DLSS5 ReShade AIO", manifest_tag(&m), &latest.aio);
    }
    if let Ok(m) = fs::read_to_string(dir.join(game::OPTI_MANIFEST)) {
        // Compare against the repo this install came from. Comparing a pre-SR
        // tag (v0.7.7) with the stable build's (v0.2.0-dlssnr) reported an
        // update on every run, and Install wrote the same tag back, so the
        // notice never cleared (#88). Older manifests carry no repo line; the
        // stable build's tags all end in "-dlssnr", which tells them apart.
        let want = match manifest_repo(&m) {
            Some(r) if r == OPTI_PRESR_REPO => &latest.opti_presr,
            Some(_) => &latest.opti,
            None if manifest_tag(&m).is_some_and(|t| !t.ends_with("-dlssnr")) => &latest.opti_presr,
            None => &latest.opti,
        };
        match (manifest_tag(&m), want) {
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

/// The repo an OptiScaler manifest was installed from, from its `# repo …`
/// header. Absent in manifests written before 0.13.15.
fn manifest_repo(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("# repo "))
        .map(|t| t.trim().to_owned())
}

/// The first stable release carrying a `.zip`. Both forks also publish rolling
/// "nightly" releases whose assets are `.7z`, and taking a release's first
/// asset blindly picked a checksum text file or an archive the installer
/// cannot open.
///
/// From v0.8.3 the pre-SR fork ships two zips per release — the standard build
/// and an `-rtx40-mfg` variant — and lists the MFG one first. Taking the first
/// zip would have handed the unlock build to everyone, against the author's
/// own "choose the standard ZIP unless you need the optional RTX 40 MFG
/// unlock". The variant is chosen by the MFG tick; a release with only one zip
/// still gets that one.
fn pick_opti_zip(releases: &[Value], want_mfg: bool) -> Option<String> {
    releases
        .iter()
        .filter(|r| r["prerelease"] != Value::Bool(true))
        .find_map(|r| {
            let zips: Vec<(String, String)> = r
                .get("assets")?
                .as_array()?
                .iter()
                .filter_map(|a| {
                    let url = a.get("browser_download_url")?.as_str()?;
                    let name = a.get("name")?.as_str()?.to_ascii_lowercase();
                    (name.ends_with(".zip") && !name.contains("sha256"))
                        .then(|| (name, url.to_owned()))
                })
                .collect();
            if zips.is_empty() {
                return None;
            }
            zips.iter()
                .find(|(n, _)| n.contains("-mfg") == want_mfg)
                .or_else(|| zips.first())
                .map(|(_, u)| u.clone())
        })
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
    let repo = opti_repo();
    let latest = net::latest_tag(client, repo).ok();
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
                // Current, but the ticks may have changed since — apply them.
                patch_opti_ini(st, d)?;
                return Ok(vec![format!(
                    "OptiScaler already current ({a}), settings applied"
                )]);
            }
            (Some(a), Some(b)) => progress(0, &format!("OptiScaler {a} is out, {b} available")),
            (Some(_), None) => {
                patch_opti_ini(st, d)?;
                return Ok(vec![
                    "OptiScaler present (could not check for a newer one), settings applied"
                        .to_owned(),
                ]);
            }
            (None, _) => progress(0, "OptiScaler version not recorded, refreshing"),
        }
    }
    // Stable release only (releases/latest skips pre-releases); the API list
    // and the releases page both put betas first.
    // Two zips per release since the pre-SR fork's v0.8.3: the standard build
    // ends in its version digit, the RTX 40 MFG variant in "-mfg". The tick
    // decides; a release with a single zip matches the fallback either way.
    let want_mfg = ada_mfg() == "true";
    let by_name = |tag: &str| -> Result<String> {
        let specific = if want_mfg {
            r#"[^"]+-mfg\.zip"#
        } else {
            r#"[^"]+\d\.zip"#
        };
        net::github_asset_url_html(client, repo, tag, specific)
            .or_else(|_| net::github_asset_url_html(client, repo, tag, r#"[^"]+\.zip"#))
    };
    let asset: String = match latest.clone() {
        Some(tag) => by_name(&tag)?,
        None => match net::get_json_github(client, &opti_releases_url()) {
            Ok(releases) => releases
                .as_array()
                .and_then(|a| pick_opti_zip(a, want_mfg))
                .ok_or_else(|| anyhow!("{repo} has no release asset"))?,
            Err(_) => {
                let tags = net::github_release_tags_html(client, repo, "v", 2)?;
                let tag = tags
                    .first()
                    .ok_or_else(|| anyhow!("no {repo} release found"))?;
                by_name(tag)?
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
    // OptiScaler ships DLSS Neural Rendering off, and its overlay toggle lives
    // only in memory unless the user finds the Save button -- so the whole
    // point of this install had to be switched back on at every launch.
    patch_opti_ini(st, d)?;
    // The repo goes in beside the tag: the two builds number their releases
    // independently, so a tag alone cannot say whether v0.7.7 is current (#88).
    let header = latest
        .as_deref()
        .map(|t| format!("# tag {t}\n# repo {}\n", opti_repo()))
        .unwrap_or_default();
    fs::write(
        d.join(game::OPTI_MANIFEST),
        format!("{header}{}", installed.join("\n")),
    )?;
    installed.push(game::OPTI_MANIFEST.into());
    Ok(installed)
}

/// Apply this install's choices to `OptiScaler.ini`: neural rendering on, the
/// model resolution, and the optional frame-generation switches.
///
/// Called on a fresh install and again when the package is already current —
/// the ticks are the user's, and until this ran on the "already current"
/// path too, changing Model Resolution or ticking frame generation on an
/// up-to-date install wrote nothing at all.
fn patch_opti_ini(st: &GameStatus, d: &Path) -> Result<()> {
    let ini = d.join(OPTI_INI);
    if let Ok(text) = fs::read_to_string(&ini) {
        let mut cur = text;
        if let Some(patched) = set_dlss_nr_enabled(&cur) {
            cur = patched;
        }
        // The frame stays full size; only the model's own work is done small and
        // enlarged, and its cost falls with the square of this. The single
        // biggest performance lever on this route.
        if let Some(patched) = set_ini_key(&cur, "DlssNr", "WorkingScale", &working_scale()) {
            cur = patched;
        }
        // RTX 40 multi-frame generation. This one is built into the fork and
        // memory-only — no file to fetch, nothing to sideload — so it is a
        // setting we can honestly turn on for someone. The Ampere/Turing
        // equivalent in the same ini sideloads a DLL that has no published
        // release, so it is deliberately not offered (#83).
        //
        // The key lives under [DLSSG], not [FrameGen] — v0.13.12 through
        // v0.13.14 wrote it into the wrong section, where OptiScaler never
        // read it. From the fork's v0.8.3 the key exists only in the
        // -rtx40-mfg package, which the tick now selects; on the standard
        // package this line appends a key nothing reads, which is harmless.
        if let Some(patched) = set_ini_key(&cur, "DLSSG", "AdaMfgUnlock", ada_mfg()) {
            cur = patched;
        }
        // OptiScaler's own frame generation: FSR 3.1 interpolation over the
        // upscaler it already runs, 2X, on any RTX card. Every library it
        // needs ships in the package, so it is four keys. D3D12 only — every
        // FG output in that ini is a D3D12 component. HUDFix is what the ini
        // itself asks for with the upscaler as input ("To prevent UI
        // glitching, Hudfix is required"). Off means untouched: the ini
        // carries the user's own frame-generation choice from the overlay,
        // and a refresh must not undo it.
        if opti_fg() && matches!(st.api, game::Api::Dx12 | game::Api::Unknown) {
            for (section, key, value) in [
                ("FrameGen", "Enabled", "true"),
                ("FrameGen", "FGInput", "upscaler"),
                ("FrameGen", "FGOutput", "fsrfg"),
                ("OptiFG", "HUDFix", "true"),
            ] {
                if let Some(patched) = set_ini_key(&cur, section, key, value) {
                    cur = patched;
                }
            }
        }
        // RE Engine trips its own scheduler assertion unless the compute root
        // signature is put back, and fights REFramework over WndProc unless
        // input is polled. The graphics-side restores must stay off there: they
        // hand dangling descriptors to the NVIDIA driver when the swapchain is
        // recreated after the intro, which is a crash in nvwgf2umx.dll
        // (#44, Dragon's Dogma 2).
        if st.re_engine {
            for (key, value) in [
                ("ManualInputPolling", "true"),
                ("RestoreComputeSignature", "true"),
                ("RestoreGraphicSignature", "false"),
                ("ExtendedStateRestore", "false"),
            ] {
                if let Some(patched) = set_ini_key(&cur, "Hotfix", key, value) {
                    cur = patched;
                }
            }
        }
        fs::write(&ini, cur)?;
    }
    Ok(())
}

/// Remove an OptiScaler install recorded in the manifest.
fn uninstall_opti(d: &Path, removed: &mut Vec<String>) -> Result<()> {
    uninstall_manifest(
        d,
        game::OPTI_MANIFEST,
        &["OptiScaler/D3D12_OptiScaler", "OptiScaler", "Licenses"],
        removed,
    )
}

/// Remove every file listed in `manifest_name`, then the manifest itself and
/// any of `empty_dirs` the archive created that are now empty.
fn uninstall_manifest(
    d: &Path,
    manifest_name: &str,
    empty_dirs: &[&str],
    removed: &mut Vec<String>,
) -> Result<()> {
    let manifest = d.join(manifest_name);
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
    for sub in empty_dirs {
        let p = d.join(sub.replace('/', std::path::MAIN_SEPARATOR_STR));
        if p.is_dir() && fs::read_dir(&p)?.next().is_none() {
            fs::remove_dir(&p)?;
        }
    }
    fs::remove_file(&manifest)?;
    removed.push(manifest_name.into());
    Ok(())
}

fn uninstall_aio(d: &Path, removed: &mut Vec<String>) -> Result<()> {
    uninstall_manifest(d, game::AIO_MANIFEST, &["licenses"], removed)
}

pub const BRIDGE_DOWNLOAD: &str =
    "https://github.com/NIGos/dlss5-bridge/releases/latest/download/dlss5-bridge.addon64";
/// matiasLombo/neural-upstream: the neural consumer that runs the network at the
/// game's render resolution instead of at output resolution, replacing the
/// RenoDX DLSS 5 add-on rather than joining it.
const UPSTREAM_DOWNLOAD: &str =
    "https://github.com/matiasLombo/neural-upstream/releases/latest/download/nvngx.dll.addon64";
/// mavismmg/MFGAdaUnlock-RenoDx: RTX 40 multi-frame generation as a ReShade
/// add-on. DangerousBerries reached 6X with this where OptiScaler's own
/// built-in unlock reported "DLSSG not patched: capability not matched" (#83).
const MFG_DOWNLOAD: &str = "https://github.com/mavismmg/MFGAdaUnlock-RenoDx/releases/latest/download/renodx-mfgunlock.addon64";
pub const RHI_RELEASES: &str =
    "https://api.github.com/repos/RankFTW/rhi-repo/releases?per_page=100";
pub const RHI_REPO: &str = "RankFTW/rhi-repo";
pub const OPTI_REPO: &str = "Dagherbou/OptiScaler_DLSSNR";
/// wilsjo2's fork: the neural pass runs before super resolution instead of
/// after it, with 1-3 configurable passes. Same zip layout as Dagherbou's, so
/// it installs through the same step (#72).
pub const OPTI_PRESR_REPO: &str = "wilsjo2/OptiScaler-DLSSNR-PreSR-Multipass";

/// Which OptiScaler build to install; unset means Dagherbou's.
pub const OPTI_SOURCE_ENV: &str = "DLSS5ONECLICK_OPTI_SOURCE";

/// True when the pre-SR multipass fork was asked for.
pub fn opti_presr() -> bool {
    std::env::var(OPTI_SOURCE_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("presr"))
}

pub fn opti_repo() -> &'static str {
    if opti_presr() {
        OPTI_PRESR_REPO
    } else {
        OPTI_REPO
    }
}

fn opti_releases_url() -> String {
    format!("https://api.github.com/repos/{}/releases", opti_repo())
}

#[derive(Clone, Copy)]
pub struct Step {
    pub name: &'static str,
    pub run: fn(&Client, &GameStatus, &Path, Progress) -> Result<Vec<String>>,
}

const STEP_RESHADE: Step = Step {
    name: "ReShade (add-on build)",
    run: step_reshade,
};
const STEP_DGVOODOO: Step = Step {
    name: "dgVoodoo 2.87.3 (DX9 → D3D11)",
    run: step_dgvoodoo,
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
const STEP_MFG: Step = Step {
    name: "RTX 40 multi-frame generation add-on",
    run: step_mfg,
};
const STEP_UPSTREAM: Step = Step {
    name: "Neural Upstream add-on (experimental)",
    run: step_upstream,
};
const STEP_CONFIG: Step = Step {
    name: "ReShade config",
    run: step_config,
};
const STEP_FEEDER_CLEANUP: Step = Step {
    name: "Remove DLSS5-Feeder (game has native DLSS)",
    run: step_feeder_cleanup,
};
const STEP_DLSS5_CLEANUP: Step = Step {
    name: "Remove the RenoDX DLSS 5 add-on (Neural Upstream replaces it)",
    run: step_dlss5_cleanup,
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

/// Fraction of the frame the DLSS 5 model works at, as OptiScaler's
/// `[DlssNr] WorkingScale` wants it. Set through the UI; `1` when unset.
pub const WORKING_SCALE_ENV: &str = "DLSS5ONECLICK_WORKING_SCALE";

/// Reads the chosen model resolution, falling back to full size.
fn working_scale() -> String {
    std::env::var(WORKING_SCALE_ENV)
        .ok()
        .filter(|v| v.parse::<f32>().is_ok_and(|f| (0.25..=2.0).contains(&f)))
        .unwrap_or_else(|| "1.0".to_owned())
}

/// `[DlssNr] Enabled=true` in OptiScaler.ini; `None` when it already says so.
/// Section-scoped: `Enabled` appears under half a dozen headings in that file.
pub fn set_dlss_nr_enabled(ini: &str) -> Option<String> {
    set_ini_key(ini, "DlssNr", "Enabled", "true")
}

/// Set `key=value` inside `[section]`, appending the section or the key when
/// missing; `None` when it already reads that way. Section-scoped because
/// OptiScaler.ini repeats names like `Enabled` under many headings.
pub fn set_ini_key(ini: &str, section: &str, key: &str, value: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut out = String::with_capacity(ini.len() + 32);
    let mut in_section = false;
    let mut seen = false;
    let mut changed = false;
    for line in ini.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\r', '\n']);
        let t = raw.trim();
        if t.starts_with('[') {
            in_section = t.eq_ignore_ascii_case(&header);
        } else if in_section && t.split('=').next().unwrap_or("").trim() == key {
            seen = true;
            if t.split('=').nth(1).map(str::trim) != Some(value) {
                out.push_str(&format!("{key}={value}"));
                out.push_str(&line[raw.len()..]);
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
        out.push_str(&format!("\n{header}\n{key}={value}\n"));
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
pub fn plan_with(st: &GameStatus, engine: Engine, with_renodx: bool, upstream: bool) -> Vec<Step> {
    let mut v = if engine == Engine::Aio {
        // The AIO is the whole consumer: ReShade to load it, the model and
        // NVIDIA's runtimes beside it. No Feeder, no RenoDX add-on.
        let mut v = vec![STEP_RESHADE, STEP_AIO, STEP_DLSSNR_ONLY, STEP_AIO_RUNTIME];
        if with_renodx {
            v.push(STEP_RENODX);
        }
        v.push(STEP_AIO_CONFIG);
        v
    } else if engine == Engine::Opti {
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
        let mut v = plan_reshade(st, upstream);
        if with_renodx {
            let at = v.len() - 1; // before ReShade config
            v.insert(at, STEP_RENODX);
        }
        v
    };
    // RTX 40 multi-frame generation. On the OptiScaler route the fork writes
    // its own ini key; on the ReShade route it is this separate add-on, which
    // is what actually reached 6X for the reporter in #83. It is an .addon64,
    // so a 32-bit game's ReShade could not load it.
    if engine == Engine::ReShade && ada_mfg() == "true" && !st.is32() {
        let at = v.len().saturating_sub(1); // before ReShade config
        v.insert(at, STEP_MFG);
    }
    if st.re_engine {
        v.insert(0, STEP_REFRAMEWORK);
    }
    // DX9 never loads dxgi.dll; dgVoodoo must sit in the game folder first.
    // Always run on Dx9 (even when the DLL is already present) so Install can
    // refresh dgVoodoo.conf — Uninstall never removes dgVoodoo, and a bare
    // OutputAPI-only conf leaves stock VRAM=256 (Gothic 3 texture failures).
    if st.api == game::Api::Dx9 {
        v.insert(0, STEP_DGVOODOO);
    }
    v.push(STEP_GPU_PREF);
    v
}

fn plan_reshade(st: &GameStatus, upstream: bool) -> Vec<Step> {
    let mut v = plan_reshade_consumer(st, upstream);
    // Two neural consumers in one ReShade would both create NGX features on
    // the same frame; the AIO goes when this route takes over.
    if st.aio {
        v.insert(1, STEP_AIO_CLEANUP);
    }
    v
}

fn plan_reshade_consumer(st: &GameStatus, upstream: bool) -> Vec<Step> {
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
            // Neural Upstream is itself the neural consumer: it creates the
            // DLSSNR feature and needs only the model beside it, so it takes
            // the RenoDX add-on's place rather than sitting next to it.
            if upstream {
                v.push(STEP_DLSS5_CLEANUP);
                v.push(STEP_UPSTREAM);
                v.push(STEP_DLSSNR_ONLY);
            } else {
                v.push(STEP_DLSS5);
            }
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
/// Tag of the DLSS 5 add-on build to install; unset means the newest one.
pub const RENODX_TAG_ENV: &str = "DLSS5ONECLICK_RENODX_TAG";

/// The classic-engine add-on. The Feeder's own host measured v4.7 to fault
/// inside the driver's NGX runtime on NVIDIA 616.64 — an access violation in
/// D3D12Core.dll reached through nvngx_dlssnr.dll — and names this build as one
/// that passes there (#69).
pub const RENODX_CLASSIC_TAG: &str = "renodx-dlss5-4.55";

/// A pinned add-on build, when one was asked for: `(tag, url)`.
fn rhi_pinned(client: &Client, prefix: &str) -> Option<Result<(String, String)>> {
    if prefix != "renodx-dlss5-" {
        return None;
    }
    let tag = std::env::var(RENODX_TAG_ENV).ok()?;
    if tag.is_empty() {
        return None;
    }
    Some(
        net::github_asset_url_html(client, RHI_REPO, &tag, r#"[^"]+\.zip"#)
            .map(|url| (tag.clone(), url))
            .with_context(|| format!("DLSS 5 add-on build {tag} not found on {RHI_REPO}")),
    )
}

pub fn rhi_latest(client: &Client, prefix: &str) -> Result<(String, String)> {
    if let Some(pinned) = rhi_pinned(client, prefix) {
        return pinned;
    }
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

/// NVIDIA's own DLSS repository: `nvngx_dlss.dll` and `nvngx_dlssg.dll` sit
/// in it at every release tag, byte-identical to the copies rhi-repo mirrors
/// (checked at v310.9.1: same size, same SHA-256 for both). Fetching from the
/// publisher answers the provenance question the mirror could not.
pub const NVIDIA_DLSS_REPO: &str = "NVIDIA/DLSS";

/// The newest NVIDIA/DLSS release tag and the raw URL of `name` under it —
/// `(tag, url)`. `None` when the lookup fails; callers fall back to the mirror.
pub fn nvidia_dll(client: &Client, name: &str) -> Option<(String, String)> {
    let tag = net::latest_tag(client, NVIDIA_DLSS_REPO).ok()?;
    let url = format!(
        "https://raw.githubusercontent.com/{NVIDIA_DLSS_REPO}/{tag}/lib/Windows_x86_64/rel/{name}"
    );
    Some((tag, url))
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

/// Parse a loose dgVoodoo-style INI and ensure Feeder-safe keys without wiping CPL settings.
/// - Force `OutputAPI = d3d11_fl11_0` under `[General]`
/// - Floor `VRAM` under `[DirectX]` to at least [`DGVOODOO_VRAM_FLOOR`]
/// - Create missing sections/keys; leave every other line untouched
fn merge_dgvoodoo_conf(existing: &str) -> String {
    let mut out = String::with_capacity(existing.len() + 128);
    let mut section = String::new();
    let mut saw_general = false;
    let mut saw_directx = false;
    let mut output_api_set = false;
    let mut vram_set = false;
    let mut watermark_set = false;

    for raw in existing.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
            // Flush required keys before leaving a section.
            if section.eq_ignore_ascii_case("General") && !output_api_set {
                out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
                output_api_set = true;
            }
            if section.eq_ignore_ascii_case("DirectX") {
                if !vram_set {
                    out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
                    vram_set = true;
                }
                if !watermark_set {
                    out.push_str("dgVoodooWatermark = false\n");
                    watermark_set = true;
                }
            }
            section = trimmed[1..trimmed.len() - 1].to_string();
            if section.eq_ignore_ascii_case("General") {
                saw_general = true;
            }
            if section.eq_ignore_ascii_case("DirectX") {
                saw_directx = true;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if section.eq_ignore_ascii_case("General") && key.eq_ignore_ascii_case("OutputAPI") {
                out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
                output_api_set = true;
                continue;
            }
            if section.eq_ignore_ascii_case("DirectX") && key.eq_ignore_ascii_case("VRAM") {
                let cur = val
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let floor = cur.max(DGVOODOO_VRAM_FLOOR);
                out.push_str(&format!("VRAM = {floor}\n"));
                vram_set = true;
                continue;
            }
            if section.eq_ignore_ascii_case("DirectX")
                && key.eq_ignore_ascii_case("dgVoodooWatermark")
            {
                out.push_str("dgVoodooWatermark = false\n");
                watermark_set = true;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if section.eq_ignore_ascii_case("General") && !output_api_set {
        out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
        output_api_set = true;
    }
    if section.eq_ignore_ascii_case("DirectX") {
        if !vram_set {
            out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
            vram_set = true;
        }
        if !watermark_set {
            out.push_str("dgVoodooWatermark = false\n");
            watermark_set = true;
        }
    }

    if !saw_general {
        out.push_str("\n[General]\n");
        out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
        output_api_set = true;
    } else if !output_api_set {
        // Section existed but key never appeared (empty section mid-file already handled).
        out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
    }

    if !saw_directx {
        out.push_str("\n[DirectX]\n");
        out.push_str("VideoCard = geforce_9800_gt\n");
        out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
        out.push_str("dgVoodooWatermark = false\n");
        out.push_str("Antialiasing = appdriven\n");
        out.push_str("FastVideoMemoryAccess = false\n");
    } else {
        if !vram_set {
            out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
        }
        if !watermark_set {
            out.push_str("dgVoodooWatermark = false\n");
        }
    }

    let _ = (output_api_set, vram_set);
    out
}

fn assert_dgvoodoo_conf_healthy(text: &str) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("outputapi") || !lower.contains("d3d11_fl11_0") {
        bail!("dgVoodoo.conf health check failed: OutputAPI must be d3d11_fl11_0");
    }
    // Find VRAM value
    let mut vram_ok = false;
    let mut section = "";
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            section = t;
            continue;
        }
        if section.eq_ignore_ascii_case("[DirectX]") {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case("VRAM") {
                    let n = v
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    vram_ok = n >= DGVOODOO_VRAM_FLOOR;
                }
            }
        }
    }
    if !vram_ok {
        bail!("dgVoodoo.conf health check failed: VRAM must be >= {DGVOODOO_VRAM_FLOOR}");
    }
    Ok(())
}

/// Smart-merge (or create) `dgVoodoo.conf`: force OutputAPI, floor VRAM, preserve the rest.
/// Writes `dgVoodoo.conf.bak` once before the first edit of an existing file.
pub fn write_dgvoodoo_conf(game_dir: &Path) -> Result<()> {
    let conf = game_dir.join("dgVoodoo.conf");
    let bak = game_dir.join("dgVoodoo.conf.bak");
    let text = if conf.is_file() {
        let existing =
            fs::read_to_string(&conf).with_context(|| format!("reading {}", conf.display()))?;
        if !bak.is_file() {
            fs::write(&bak, &existing).with_context(|| format!("writing {}", bak.display()))?;
        }
        merge_dgvoodoo_conf(&existing)
    } else {
        DGVOODOO_CONF_TEMPLATE.to_string()
    };
    assert_dgvoodoo_conf_healthy(&text)?;
    fs::write(&conf, text).with_context(|| format!("writing {}", conf.display()))?;
    Ok(())
}

fn dgvoodoo_d3d9_member(bitness: u8) -> &'static str {
    if bitness == 64 {
        DGVOODOO_D3D9_MEMBER_X64
    } else {
        DGVOODOO_D3D9_MEMBER_X86
    }
}

/// Place official dgVoodoo `MS/{x86|x64}/D3D9.dll` + smart-merged conf in the game folder.
/// Never restores `d3d9.dll.off` (old ReShade); always extracts from the release zip.
pub fn install_dgvoodoo_from_zip(
    zip_path: &Path,
    game_dir: &Path,
    bitness: u8,
) -> Result<Vec<String>> {
    let want = dgvoodoo_d3d9_member(bitness);
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("dgVoodoo download is not a valid zip")?;
    let member = zip
        .file_names()
        .find(|n| {
            let norm = n.replace('\\', "/");
            norm.eq_ignore_ascii_case(want)
                || (bitness != 64 && norm.to_ascii_lowercase().ends_with("/ms/x86/d3d9.dll"))
                || (bitness == 64 && norm.to_ascii_lowercase().ends_with("/ms/x64/d3d9.dll"))
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("dgVoodoo zip does not contain {want} — unexpected release layout")
        })?;
    let dest = game_dir.join("d3d9.dll");
    // Refuse to clobber a foreign wrapper; callers should have blocked Install already.
    if dest.is_file() && !game::is_dgvoodoo(game_dir) {
        bail!(
            "a d3d9.dll that is not dgVoodoo is already present; remove or replace it, then Install again"
        );
    }
    let had_conf = game_dir.join(game::DGVOODOO_CONF).is_file();
    net::extract_member(&mut zip, &member, &dest)?;
    write_dgvoodoo_conf(game_dir)?;
    if !game::is_dgvoodoo(game_dir) {
        bail!("wrote d3d9.dll + dgVoodoo.conf but dgVoodoo was not detected afterward");
    }
    // Record what this tool put there so Remove can take it away again (#91).
    let mut marker = format!("{DGVOODOO_TAG}\n");
    if !had_conf {
        marker.push_str("conf-ours\n");
    }
    fs::write(game_dir.join(game::DGVOODOO_MARKER), marker)?;
    Ok(vec!["d3d9.dll".into(), "dgVoodoo.conf".into()])
}

fn step_dgvoodoo(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let mut out: Vec<String> = Vec::new();
    let member = dgvoodoo_d3d9_member(st.bitness);
    if game::is_dgvoodoo(d) {
        progress(50, "dgVoodoo DLL present — merging conf");
    } else {
        // Do not treat d3d9.dll.off (old ReShade) as dgVoodoo — download the real DLL.
        if d.join("d3d9.dll").is_file() {
            bail!(
                "a d3d9.dll that is not dgVoodoo is already present; remove or replace it with \
                 dgVoodoo 2.87.3 ({member}), then Install again"
            );
        }
        progress(0, &format!("Downloading dgVoodoo {DGVOODOO_TAG}"));
        let z = work.join("dgVoodoo2_87_3.zip");
        net::download(client, DGVOODOO_ZIP, &z, "dgVoodoo 2.87.3", progress)?;
        progress(90, &format!("Extracting {member}"));
        out.extend(install_dgvoodoo_from_zip(&z, d, st.bitness)?);
        progress(100, "dgVoodoo 2.87.3 ready");
        return Ok(out);
    }
    // DLL already there: merge conf so VRAM/OutputAPI stay safe without wiping CPL.
    write_dgvoodoo_conf(d)?;
    out.push("dgVoodoo.conf (OutputAPI/VRAM merged)".into());
    progress(100, "dgVoodoo conf merged");
    Ok(out)
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
    // A proxy of the wrong bitness is invisible from inside the game: the
    // loader simply does not load it, ReShade writes no log, and the Home key
    // does nothing. Whatever the marker says, that one gets replaced (#69).
    let wrong_bitness = st.reshade && game::exe_bitness(&proxy).is_ok_and(|b| b != st.bitness);
    progress(0, "Looking up latest ReShade");
    let (ver, url) = resolve_reshade_setup(client)?;
    if st.reshade && !wrong_bitness {
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
    let mut out = install_reshade_from_setup(&setup, d, st.bitness, game::RESHADE_PROXY)?;
    fs::write(d.join(game::RESHADE_MARKER), ver.as_bytes())?;
    if wrong_bitness {
        out.push(format!(
            "{} was {}-bit in a {}-bit game and has been replaced",
            game::RESHADE_PROXY,
            if st.bitness == 32 { 64 } else { 32 },
            st.bitness
        ));
        // The 64-bit add-on cannot belong to a 32-bit game either; it came from
        // the same mistaken install and ReShade would keep trying to load it.
        let stray = d.join(game::FEEDER_ADDON);
        if st.is32() && stray.is_file() {
            fs::remove_file(&stray)?;
            out.push(format!(
                "{} removed (64-bit add-on in a 32-bit game)",
                game::FEEDER_ADDON
            ));
        }
    }
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
    // The 32-bit halves talk a versioned IPC protocol to each other, and a
    // mismatch is fatal at runtime: "the game add-on speaks protocol v8, this
    // host v9 -- the two halves are from different releases" and the host exits
    // (#69). Sizes cannot see that, so the tag each half was taken from is
    // recorded and both are replaced unless both markers name this release.
    let marker_says = |dir: &Path| -> bool {
        fs::read_to_string(dir.join(game::FEEDER_MARKER)).is_ok_and(|m| m.trim() == tag.as_str())
    };
    let halves_agree = !st.is32() || marker_says(&st.consumer_dir());
    let host_current = match &host_member {
        Some(m) => same_size(&mut zip, m, &st.consumer_dir().join(game::HOST_EXE)),
        None => true,
    };
    if st.feeder
        && halves_agree
        && marker_says(d)
        && host_current
        && same_size(&mut zip, &addon, &d.join(addon_name))
    {
        return Ok(vec![format!("DLSS5-Feeder already current ({tag}{note})")]);
    }
    net::extract_member(&mut zip, &addon, &d.join(addon_name))?;
    fs::write(d.join(game::FEEDER_MARKER), tag.as_bytes())?;
    let mut out = vec![format!("{addon_name} ({tag}{note})")];
    if let Some(m) = &host_member {
        let host = st.consumer_dir();
        fs::create_dir_all(&host)?;
        net::extract_member(&mut zip, m, &host.join(game::HOST_EXE))?;
        // Both halves now carry the tag they came from, so a later install can
        // tell "same release" from "same size".
        fs::write(host.join(game::FEEDER_MARKER), tag.as_bytes())?;
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
    if let Some(msg) = apply_traa_ui_patch(game_dir)? {
        installed.push(msg);
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
        let mut out = vec![];
        if let Some(msg) = apply_traa_ui_patch(st.game_dir())? {
            out.push(msg);
        }
        return Ok(out);
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
        (
            "renodx-dlss5-",
            game::DLSS5_ADDON,
            false,
            Some(game::DLSS5_ADDON_MARKER),
        ),
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
    // This machine has already been told, by the Feeder's own host, that the
    // current add-on build faults in its driver. Fetching that build again just
    // reproduces it, so take the one the host names as passing — unless the
    // user pinned a build themselves, in which case that wins (#69).
    // A 32-bit game was excluded here for no reason I can defend: its add-on
    // lives in host64\ and is fetched by this same loop, and its host is the
    // very thing that prints the verdict. sempie27's GTA IV kept getting v4.7
    // back while its own log said v4.7 faults on this driver (#69).
    let auto_classic = std::env::var_os(RENODX_TAG_ENV).is_none() && addon_faulted_in_driver(&cdir);
    if auto_classic {
        std::env::set_var(RENODX_TAG_ENV, RENODX_CLASSIC_TAG);
    }
    let mut installed = Vec::new();
    if auto_classic {
        installed.push(format!(
            "{RENODX_CLASSIC_TAG}: this game's log reports the newer build faulting in the driver"
        ));
    }
    for (prefix, fname, present, marker) in plan {
        // NVIDIA's own DLL comes from NVIDIA's own repository when it can be
        // reached; the mirror is the fallback, not the source.
        let direct = fname == game::DLSS_DLL;
        let (tag, url) = match (direct, nvidia_dll(client, fname)) {
            (true, Some(t)) => t,
            _ => rhi_latest(client, prefix)?,
        };
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
        let dest = cdir.join(fname);
        // A raw DLL from NVIDIA's repository is the file itself, not a zip.
        if url.ends_with(".dll") {
            let tmp = work.join(fname);
            net::download(client, &url, &tmp, fname, progress)?;
            if game::exe_bitness(&tmp).ok() != Some(64) {
                bail!("{fname} from {NVIDIA_DLSS_REPO} {tag} is not a 64-bit Windows DLL");
            }
            fs::copy(&tmp, &dest)?;
            if let Some(m) = marker {
                fs::write(cdir.join(m), tag.as_bytes())?;
            }
            installed.push(format!("{fname} ({NVIDIA_DLSS_REPO} {tag})"));
            continue;
        }
        let z = work.join(format!("{tag}.zip"));
        net::download(client, &url, &z, fname, progress)?;
        if fname == game::DLSS5_ADDON && st.dlss5_addon {
            let f = fs::File::open(&z)?;
            let mut zip =
                zip::ZipArchive::new(f).context("DLSS 5 add-on download is not a valid zip")?;
            let hit = zip
                .file_names()
                .find(|n| net::file_name(n).eq_ignore_ascii_case(fname))
                .map(str::to_owned);
            if hit.is_some_and(|h| same_size(&mut zip, &h, &dest)) {
                // Record the tag even when nothing is copied: every build of
                // this add-on carries the same FileVersion, so the tag on disk
                // is the only way anything afterwards can name the build.
                if let Some(m) = marker {
                    let _ = fs::write(cdir.join(m), tag.as_bytes());
                }
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
    // The pin belongs to this game, not to the session: leaving it set would
    // quietly hold the next game on the classic build too.
    if auto_classic {
        std::env::remove_var(RENODX_TAG_ENV);
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

// ── aio engine: kibblerz's standalone add-on, whole zip beside the exe ──

/// Extract the 64-bit AIO release into the game folder and record every path
/// in a manifest, tag in the header, for refresh and Remove. Any other neural
/// consumer this tool placed goes first: two of them in one ReShade would each
/// create NGX features on the same frame.
fn step_aio(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    progress(0, "Looking up DLSS5 ReShade AIO releases");
    let tag = net::latest_tag(client, AIO_REPO)?;
    let mut installed = Vec::new();
    for (name, marker) in [
        (game::FEEDER_ADDON, Some(game::FEEDER_MARKER)),
        (game::DLSS5_ADDON, Some(game::DLSS5_ADDON_MARKER)),
        (game::UPSTREAM_ADDON, None),
        (game::BRIDGE_ADDON, None),
        (game::MFG_ADDON, None),
    ] {
        for f in std::iter::once(name).chain(marker) {
            let p = d.join(f);
            if p.is_file() {
                fs::remove_file(&p)?;
                if f == name {
                    installed.push(format!("removed {f} (the AIO replaces it)"));
                }
            }
        }
    }
    if let Ok(m) = fs::read_to_string(d.join(game::AIO_MANIFEST)) {
        if st.aio && manifest_tag(&m).as_deref() == Some(tag.as_str()) {
            installed.push(format!("{} already current ({tag})", game::AIO_ADDON));
            return Ok(installed);
        }
        progress(0, &format!("DLSS5 ReShade AIO: {tag} is out, refreshing"));
    }
    let url = net::github_asset_url_html(client, AIO_REPO, &tag, r#"[^"]+-64-bit\.zip"#)?;
    let zip_path = work.join("dlss5-aio.zip");
    net::download(client, &url, &zip_path, "DLSS5 ReShade AIO", progress)?;
    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("AIO download is not a valid zip")?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let mut written: Vec<String> = Vec::new();
    for member in names {
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
        let out_rel = parts.join("/");
        let dest = d.join(out_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        net::extract_member(&mut zip, &member, &dest)?;
        written.push(out_rel);
    }
    if !written.iter().any(|p| p == game::AIO_ADDON) {
        bail!(
            "the AIO release had no {} — layout changed upstream",
            game::AIO_ADDON
        );
    }
    fs::write(
        d.join(game::AIO_MANIFEST),
        format!("# tag {tag}\n# repo {AIO_REPO}\n{}", written.join("\n")),
    )?;
    installed.push(format!("{} ({tag})", game::AIO_ADDON));
    installed.extend(written.into_iter().filter(|p| p != game::AIO_ADDON));
    installed.push(game::AIO_MANIFEST.into());
    Ok(installed)
}

/// `nvngx_dlss.dll` and `nvngx_dlssg.dll` beside the AIO: the add-on creates
/// its own super-resolution and frame-generation features, so both must be in
/// the folder even in a game that never shipped them. One the game did ship is
/// left alone.
fn step_aio_runtime(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let mut out = Vec::new();
    for (name, marker, prefix) in [
        (game::DLSS_DLL, game::DLSS_MARKER, "dlss-"),
        (game::DLSSG_DLL, game::DLSSG_MARKER, "dlssg-"),
    ] {
        let dest = d.join(name);
        let marker = d.join(marker);
        if dest.is_file() && !marker.is_file() {
            out.push(format!("{name} present (not placed by this tool)"));
            continue;
        }
        progress(0, &format!("Looking up {name} releases"));
        let (tag, url) = match nvidia_dll(client, name) {
            Some(t) => t,
            None => rhi_latest(client, prefix)?,
        };
        if dest.is_file() && fs::read_to_string(&marker).is_ok_and(|t| t.trim() == tag) {
            out.push(format!("{name} already current ({tag})"));
            continue;
        }
        if url.ends_with(".dll") {
            let tmp = work.join(name);
            net::download(client, &url, &tmp, name, progress)?;
            if game::exe_bitness(&tmp).ok() != Some(64) {
                bail!("{name} from {NVIDIA_DLSS_REPO} {tag} is not a 64-bit Windows DLL");
            }
            fs::copy(&tmp, &dest)?;
        } else {
            let z = work.join(format!("{tag}.zip"));
            net::download(client, &url, &z, name, progress)?;
            install_single_from_zip(&z, name, &dest)?;
        }
        fs::write(&marker, tag.as_bytes())?;
        out.push(format!("{name} ({tag})"));
    }
    Ok(out)
}

/// ReShade.ini for the AIO: the add-on carries its own settings, so only the
/// ini itself and a cleared disabled-add-ons list.
fn step_aio_config(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    reshade_ini::write_reshade_ini(st.game_dir())?;
    reshade_ini::clear_disabled_addons(st.game_dir())?;
    progress(100, "ReShade.ini written");
    Ok(vec![game::RESHADE_INI.into()])
}

fn step_aio_cleanup(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    progress(0, "Removing the standalone AIO add-on");
    let mut removed = Vec::new();
    uninstall_aio(st.game_dir(), &mut removed)?;
    Ok(removed)
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

/// The plan already declines to *install* the RenoDX add-on on the Neural
/// Upstream route, but nothing removed one an earlier run had placed. Anyone who
/// installed once without the box and again with it ends up with both, and
/// ReShade loads every add-on it finds.
///
/// They are not additive. Both detour `NVSDK_NGX_D3D12_CreateFeature` and
/// `EvaluateFeature`, and both create NGX feature 18 on the same device. The
/// second create is refused, so the user gets no neural rendering at all rather
/// than one of the two implementations.
fn step_dlss5_cleanup(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let f = st.consumer_dir().join(game::DLSS5_ADDON);
    if f.is_file() {
        fs::remove_file(&f)?;
        removed.push(game::DLSS5_ADDON.to_owned());
        progress(
            100,
            "RenoDX DLSS 5 add-on removed; Neural Upstream replaces it",
        );
    } else {
        progress(100, "no RenoDX DLSS 5 add-on to remove");
    }
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

/// The RTX 40 MFG add-on, fetched only when the tick asked for it.
///
/// Like the bridge it carries no version in its file name, so an existing copy
/// is refreshed whenever the published file differs in size.
fn step_mfg(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let dest = st.game_dir().join(game::MFG_ADDON);
    if st.mfg && dest.is_file() {
        let local = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        match net::remote_len(client, MFG_DOWNLOAD) {
            Ok(Some(remote)) if remote != local => {
                progress(0, "MFG unlock changed upstream, refreshing");
            }
            Ok(_) => {
                return Ok(vec![format!("{} already current", game::MFG_ADDON)]);
            }
            Err(_) => {
                return Ok(vec![format!(
                    "{} present (could not check for a newer one)",
                    game::MFG_ADDON
                )]);
            }
        }
    } else {
        progress(0, "Fetching the RTX 40 MFG unlock");
    }
    net::download(client, MFG_DOWNLOAD, &dest, game::MFG_ADDON, progress)?;
    let mut done = vec![game::MFG_ADDON.to_owned()];
    done.extend(mfg_provider(client, st, work, progress)?);
    Ok(done)
}

/// The frame-generation provider the MFG add-on will accept.
///
/// Version 0.9 validates the provider by build and refuses the rest: a reporter
/// with an RTX 4060 got "Validated provider result: unsupported/unknown" and no
/// effect at all, on a game whose own menu offered 2X-6X (#90). The add-on's
/// README says to use the newest `nvngx_dlssg.dll`; rhi-repo publishes it, the
/// same place this tool already takes `nvngx_dlss.dll` and the neural model
/// from, so Install can place it without asking anyone to fetch a DLL.
///
/// A provider the game shipped is moved to `.original` rather than overwritten,
/// and Remove puts it back.
fn mfg_provider(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let dest = d.join(game::DLSSG_DLL);
    let marker = d.join(game::DLSSG_MARKER);
    progress(0, "Looking up frame-generation runtime releases");
    // NVIDIA's repository first (byte-identical to the mirror at v310.9.1),
    // the mirror when it cannot be reached.
    let (tag, url) = match nvidia_dll(client, game::DLSSG_DLL) {
        Some(t) => t,
        None => rhi_latest(client, "dlssg-")?,
    };
    if fs::read_to_string(&marker).is_ok_and(|t| t.trim() == tag) {
        return Ok(vec![format!("{} already current ({tag})", game::DLSSG_DLL)]);
    }
    let backup = d.join(game::DLSSG_BACKUP);
    if dest.is_file() && !marker.is_file() && !backup.is_file() {
        fs::rename(&dest, &backup)?;
    }
    if url.ends_with(".dll") {
        let tmp = work.join(game::DLSSG_DLL);
        net::download(client, &url, &tmp, game::DLSSG_DLL, progress)?;
        if game::exe_bitness(&tmp).ok() != Some(64) {
            bail!(
                "{} from {NVIDIA_DLSS_REPO} {tag} is not a 64-bit Windows DLL",
                game::DLSSG_DLL
            );
        }
        fs::copy(&tmp, &dest)?;
    } else {
        let z = work.join(format!("{tag}.zip"));
        net::download(client, &url, &z, game::DLSSG_DLL, progress)?;
        install_single_from_zip(&z, game::DLSSG_DLL, &dest)?;
    }
    fs::write(&marker, tag.as_bytes())?;
    Ok(vec![format!("{} ({tag})", game::DLSSG_DLL)])
}

// ── step 5c: neural-upstream (experimental consumer, native DLSS only) ──

fn step_upstream(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let dest = st.game_dir().join(game::UPSTREAM_ADDON);
    // Like the bridge, its releases carry no tag in the file name, so an
    // existing copy is refreshed whenever the published file differs in size.
    if st.upstream && dest.is_file() {
        let local = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        match net::remote_len(client, UPSTREAM_DOWNLOAD) {
            Ok(Some(remote)) if remote != local => {
                progress(0, "neural-upstream changed upstream, refreshing");
            }
            Ok(_) => {
                return Ok(vec![format!("{} already current", game::UPSTREAM_ADDON)]);
            }
            Err(_) => {
                return Ok(vec![format!(
                    "{} present (could not check for a newer one)",
                    game::UPSTREAM_ADDON
                )]);
            }
        }
    } else {
        progress(0, "Fetching latest neural-upstream");
    }
    net::download(
        client,
        UPSTREAM_DOWNLOAD,
        &dest,
        game::UPSTREAM_ADDON,
        progress,
    )?;
    let mut done = vec![game::UPSTREAM_ADDON.to_owned()];
    // The add-on reads its strength from ReShade.ini at startup, so the choice
    // can be made here instead of only in the in-game overlay (#68).
    let preset = upstream_preset();
    if preset != 0 {
        reshade_ini::write_upstream_preset(st.game_dir(), preset)?;
        if let Some((name, ..)) = reshade_ini::UPSTREAM_PRESETS
            .iter()
            .find(|(_, id, _)| *id == preset)
        {
            done.push(format!("neural-upstream preset: {name}"));
        }
    }
    Ok(done)
}

/// RTX 40 multi-frame generation on the OptiScaler route; unset means off.
pub const ADA_MFG_ENV: &str = "DLSS5ONECLICK_ADA_MFG";

/// `"true"` when the RTX 40 MFG unlock was asked for, else `"false"`.
///
/// The fork's own note: "Optional built-in y4my4my4m RTX 40 MFG unlock.
/// Memory-only, supported runtimes only." Memory-only is what makes it
/// offerable here — there is no second download and nothing for the user to
/// place by hand.
fn ada_mfg() -> &'static str {
    if std::env::var_os(ADA_MFG_ENV).is_some() {
        "true"
    } else {
        "false"
    }
}

/// OptiScaler's own frame generation (AMD FSR 3.1, 2X) on the OptiScaler
/// route; unset means off. The libraries it needs ship in every OptiScaler
/// package this tool installs, so it is ini keys and nothing else — which is
/// what makes it offerable on RTX 20 and 30 where NVIDIA's own is not.
pub const OPTI_FG_ENV: &str = "DLSS5ONECLICK_OPTI_FG";

fn opti_fg() -> bool {
    std::env::var_os(OPTI_FG_ENV).is_some()
}

/// Which neural-upstream strength preset to seed; 0 leaves the overlay's own.
pub const UPSTREAM_PRESET_ENV: &str = "DLSS5ONECLICK_UPSTREAM_PRESET";

fn upstream_preset() -> u8 {
    std::env::var(UPSTREAM_PRESET_ENV)
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|p| {
            reshade_ini::UPSTREAM_PRESETS
                .iter()
                .any(|(_, id, _)| id == p)
        })
        .unwrap_or(0)
}

/// Active quality resolution for the install currently running (set by `run_all_with`).
static INSTALL_QUALITY: std::sync::Mutex<Option<ResolvedQuality>> = std::sync::Mutex::new(None);

fn install_quality() -> ResolvedQuality {
    INSTALL_QUALITY
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(quality_preset::fallback_medium)
}

/// True when this game has already refused a reduced work resolution.
///
/// Some games cannot create the staging SRV for the smaller image at any size
/// below full — Dying Light fails identically at 90% and 85% and works at 100%.
/// The feed retries three times and stops, before any DLSS create, so the whole
/// install goes quiet and nothing on screen says why (#74).
pub fn work_resolution_refused(game_dir: &Path) -> bool {
    let Ok(log) = fs::read_to_string(game_dir.join("dlss5-feed.log")) else {
        return false;
    };
    if !log.contains("work-resolution staging SRV failed") {
        return false;
    }
    // Feeder 0.15.0 fixed the cause: the staging copy was created in the
    // backbuffer's exact ..._UNORM_SRGB format and then viewed as ..._UNORM,
    // which a D3D11 view may not do unless the resource is typeless, so every
    // reduced work resolution failed on an sRGB swapchain (DLSS5-Feeder#85).
    // A failure logged by an older build says nothing about the one this very
    // install is about to put in the folder, so it must not hold the setting
    // down forever.
    feeder_log_version(&log).is_none_or(|v| v >= FEEDER_SRGB_FIX)
}

/// First Feeder release where a reduced work resolution works on an sRGB
/// swapchain.
const FEEDER_SRGB_FIX: [u64; 3] = [0, 15, 0];

/// `"HH:MM:SS.mmm  dlss5-feed 0.15.0 (built ...) attached."` -> `[0, 15, 0]`.
fn feeder_log_version(log: &str) -> Option<[u64; 3]> {
    let mut it = log.lines().next()?.split_whitespace();
    it.find(|t| t.starts_with("dlss5-feed"))?;
    let raw = it.next()?;
    let mut parts = raw.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0));
    let v = [
        parts.next()?,
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    ];
    raw.chars().next()?.is_ascii_digit().then_some(v)
}

/// True when this game's own logs say the DLSS 5 add-on faulted inside the
/// driver's NGX runtime.
///
/// The Feeder's host prints that verdict itself, having measured it: the neural
/// evaluate takes an access violation in `D3D12Core.dll` reached through
/// `nvngx_dlssnr.dll`, so DLSS 5 delivers nothing while everything else keeps
/// working. It names the classic add-on build as one that passes there. Read
/// the machine's own evidence rather than assuming it from a driver number —
/// the measurement covers 616.64, and newer drivers are untested (#69).
/// Where the driver verdict is remembered once a log has stated it. A game
/// folder is the wrong place to keep it: Remove empties the folder, a fresh
/// game has no log yet, and the fault belongs to the driver rather than to any
/// one game.
fn driver_fault_flag() -> PathBuf {
    crate::settings::Settings::path().with_file_name("driver-fault.txt")
}

pub fn addon_faulted_in_driver(consumer_dir: &Path) -> bool {
    faulted_in_driver_at(
        consumer_dir,
        &driver_fault_flag(),
        crate::gpu::nvidia_driver(),
    )
}

/// The decision itself, with its two pieces of state passed in so a test can
/// exercise it without touching the machine's own file or its real driver.
fn faulted_in_driver_at(consumer_dir: &Path, flag: &Path, driver: Option<String>) -> bool {
    // On a 32-bit game the feed's log is beside the exe and the host's is in
    // host64\; on a 64-bit one both are the same folder. Check the pair either
    // way rather than assuming which layout this is.
    let dirs = [consumer_dir.to_path_buf(), consumer_dir.join("..")];
    let in_logs = dirs.iter().any(|d| {
        ["dlss5-feed.log", "dlss5-feed-host.log"].iter().any(|n| {
            fs::read_to_string(d.join(n))
                .is_ok_and(|l| l.contains("is a combination measured to fail"))
        })
    });
    let Some(drv) = driver else {
        return in_logs;
    };
    if in_logs {
        // Record it against the driver that produced it, so a driver update
        // retires the verdict by itself.
        if let Some(dir) = flag.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(flag, drv.as_bytes());
        return true;
    }
    match fs::read_to_string(flag) {
        Ok(seen) if seen.trim() == drv => true,
        Ok(_) => {
            let _ = fs::remove_file(flag);
            false
        }
        Err(_) => false,
    }
}

pub fn write_feeder_cfg(game_dir: &Path, r: &ResolvedQuality) -> Result<()> {
    let path = game_dir.join("dlss5-feed.cfg");
    // A preset that seeds a reduced work resolution would otherwise put this
    // game straight back into the failure it just came out of, every install.
    let mut r = r.clone();
    if r.work_resolution < 100 && work_resolution_refused(game_dir) {
        r.work_resolution = 100;
        r.work_upscale = 0;
        r.summary = format!(
            "{} - work_resolution held at 100% (this game refused a smaller one)",
            r.summary
        );
    }
    let r = &r;
    let mut text = quality_preset::feeder_cfg_text(r);
    // Overlay UX defaults from Settings (log_detail / evaluate_stride / …).
    let settings = crate::settings::Settings::load();
    text = crate::settings::apply_overlay_to_cfg(&text, &settings);
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ── step 6: config ─────────────────────────────────────────────────

fn step_config(_c: &Client, st: &GameStatus, _w: &Path, progress: Progress) -> Result<Vec<String>> {
    reshade_ini::write_reshade_ini(st.game_dir())?;
    reshade_ini::clear_disabled_addons(st.game_dir())?;
    if st.mode == game::Mode::Native {
        progress(100, "ReShade.ini written");
        return Ok(vec![game::RESHADE_INI.into()]);
    }
    let q = install_quality();
    reshade_ini::write_preset(st.game_dir(), q.enable_lumenite)?;
    reshade_ini::write_feed_fx_uniforms(st.game_dir(), &quality_preset::feed_fx_uniforms(&q))?;
    reshade_ini::write_traa_ui_defaults(st.game_dir())?;
    write_feeder_cfg(st.game_dir(), &q)?;
    let mut out = vec![
        game::RESHADE_INI.into(),
        game::RESHADE_PRESET.into(),
        "dlss5-feed.cfg".into(),
    ];
    if q.work_resolution < 100 && work_resolution_refused(st.game_dir()) {
        out.push(
            "work_resolution held at 100%: this game's log shows it refused a smaller one".into(),
        );
    }
    progress(100, "ReShade + feeder defaults (Optimize on first attach)");
    if let Some(msg) = apply_traa_ui_patch(st.game_dir())? {
        out.push(msg);
    }
    Ok(out)
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

/// Quality seed for an install (Settings page / CLI).
#[derive(Debug, Clone)]
pub struct InstallOpts {
    pub quality: QualityChoice,
    pub overrides: QualityOverrides,
}

impl Default for InstallOpts {
    fn default() -> Self {
        let s = crate::settings::Settings::load();
        Self {
            quality: s.quality_choice(),
            overrides: s.quality_overrides(),
        }
    }
}

pub fn run_all_with(
    exe: &Path,
    engine: Engine,
    with_renodx: bool,
    upstream: bool,
    opts: InstallOpts,
    progress: Progress,
    step_cb: &(dyn Fn(usize, usize, &str, StepState, &str) + Sync),
) -> Result<Vec<(String, Vec<String>)>> {
    let mut st = game::inspect(exe)?;
    if !st.problems.is_empty() {
        bail!("{}", st.problems.join("\n"));
    }
    if engine != Engine::Opti {
        if let Some(p) = st.reshade_engine_problem() {
            bail!("{p}");
        }
    }
    if engine == Engine::Aio && st.is32() {
        bail!(
            "The standalone AIO engine is 64-bit only here; a 32-bit game takes the Feeder path."
        );
    }
    if upstream && (engine != Engine::ReShade || st.mode != game::Mode::Native) {
        bail!(
            "Neural Upstream runs the network on the colour buffer the game hands its own DLSS, so it needs a game with DLSS of its own on the ReShade engine. This game has none - use the stable ReShade add-on."
        );
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
    let resolved = quality_preset::resolve(opts.quality, &st, &opts.overrides);
    if let Ok(mut slot) = INSTALL_QUALITY.lock() {
        *slot = Some(resolved);
    }
    let client = net::client()?;
    let work = tempfile::Builder::new()
        .prefix("dlss5oneclick-")
        .tempdir()?;
    let steps = plan_with(&st, engine, with_renodx, upstream);
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
                if let Ok(mut slot) = INSTALL_QUALITY.lock() {
                    *slot = None;
                }
                return Err(anyhow!("{}: {msg}", step.name));
            }
        }
        st = game::inspect(exe)?;
    }
    if let Ok(mut slot) = INSTALL_QUALITY.lock() {
        *slot = None;
    }
    // Re-inspect and refuse a hollow "success" when critical files are missing.
    st = game::inspect(exe)?;
    let missing = missing_install_files(&st);
    if !missing.is_empty() {
        bail!(
            "Install finished but files are missing: {}. Not reporting success.",
            missing.join(", ")
        );
    }
    Ok(results)
}

/// Convenience wrapper used by CLI / GUI when no explicit quality is passed —
/// reads `%LOCALAPPDATA%\dlss5oneclick\settings.json` for defaults.
pub fn run_all(
    exe: &Path,
    engine: Engine,
    with_renodx: bool,
    upstream: bool,
    progress: Progress,
    step_cb: &(dyn Fn(usize, usize, &str, StepState, &str) + Sync),
) -> Result<Vec<(String, Vec<String>)>> {
    let s = crate::settings::Settings::load();
    run_all_with(
        exe,
        engine,
        with_renodx,
        upstream,
        InstallOpts {
            quality: s.quality_choice(),
            overrides: s.quality_overrides(),
        },
        progress,
        step_cb,
    )
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
        d.join(game::DLSS5_ADDON_MARKER),
        d.join(game::DLSSNR_DLL),
        d.join(game::BRIDGE_ADDON),
        d.join(game::UPSTREAM_ADDON),
        d.join(game::MFG_ADDON),
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
    // The frame-generation provider: ours goes, and the game's own comes back
    // from .original if we moved it aside (#90). The restore happens below,
    // once `removed` exists, so it can be reported.
    // dgVoodoo: only a copy this tool downloaded goes, and its conf only when
    // this tool created it rather than merging into the user's own. Leaving it
    // behind meant a DX9 game that would not start still would not start after
    // Remove, with nothing naming the file responsible (#91).
    if let Ok(m) = fs::read_to_string(d.join(game::DGVOODOO_MARKER)) {
        targets.push(d.join("d3d9.dll"));
        targets.push(d.join(game::DGVOODOO_MARKER));
        if m.lines().any(|l| l.trim() == "conf-ours") {
            targets.push(d.join(game::DGVOODOO_CONF));
        }
    }
    let restore_dlssg = d.join(game::DLSSG_MARKER).is_file();
    if restore_dlssg {
        targets.push(d.join(game::DLSSG_MARKER));
        if !d.join(game::DLSSG_BACKUP).is_file() {
            targets.push(d.join(game::DLSSG_DLL));
        }
    }
    // 32-bit layout: the in-game addon32 and everything in host64\.
    targets.push(d.join(game::FEEDER_ADDON32));
    let host = d.join(game::HOST_DIR);
    if host.is_dir() {
        for f in [
            game::HOST_EXE,
            game::DLSS5_ADDON,
            game::DLSS5_ADDON_MARKER,
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
        // The two .log files are evidence, not installed files. Remove used to
        // delete them, which erased the very verdict the next Install reads to
        // decide which add-on build this machine can use (#69).
        for n in ["ReShade.ini", "ReShadePreset.ini"] {
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
    uninstall_aio(d, &mut removed)?;
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
    if restore_dlssg {
        let backup = d.join(game::DLSSG_BACKUP);
        let dll = d.join(game::DLSSG_DLL);
        if backup.is_file() {
            let _ = fs::remove_file(&dll);
            if fs::rename(&backup, &dll).is_ok() {
                removed.push(format!("{} (the game's own restored)", game::DLSSG_DLL));
            }
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

/// `uninstall`, then ReShade itself (`dxgi.dll` + ini/logs).
///
/// Refuses only when a foreign `.addon64`/`.addon32` remains — those need
/// ReShade to load. Leftover shaders under `reshade-shaders` (common on older
/// packs, e.g. Gothic 3) no longer block removal: this tool always installs
/// ReShade as `dxgi.dll`, never as `d3d9.dll`, and never deletes dgVoodoo's
/// `d3d9.dll` / `dgVoodoo.conf`. `dxgi.dll` is only deleted when it
/// verifiably is a ReShade DLL. Returns `(removed, kept_reason)`;
/// `kept_reason` is `Some` when ReShade was left.
pub fn uninstall_all(exe: &Path) -> Result<(Vec<String>, Option<String>)> {
    let mut removed = uninstall(exe)?;
    let d = exe.parent().context("exe has no parent")?;

    let mut foreign_addons: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".addon64") || n.ends_with(".addon32") {
                foreign_addons.push(n);
            }
        }
    }
    if !foreign_addons.is_empty() {
        foreign_addons.sort();
        foreign_addons.truncate(6);
        return Ok((
            removed,
            Some(format!(
                "ReShade left in place: the game still has add-ons this tool did not install ({})",
                foreign_addons.join(", ")
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
    let shaders_root = d.join("reshade-shaders");
    let mut leftover_shaders = false;
    let mut walk = vec![shaders_root.clone()];
    while let Some(dir) = walk.pop() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk.push(p);
                } else {
                    leftover_shaders = true;
                    break;
                }
            }
        }
        if leftover_shaders {
            break;
        }
    }
    if shaders_root.is_dir() {
        if leftover_shaders {
            removed.push("reshade-shaders/ (left: shaders this tool did not install)".into());
        } else {
            fs::remove_dir_all(&shaders_root)?;
            removed.push("reshade-shaders/".into());
        }
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

    /// Both OptiScaler forks publish a rolling "nightly" release whose assets
    /// are .7z, and the stable ones ship a checksum .txt beside the zip. Taking
    /// a release's first asset picked whichever happened to be listed first (#72).
    #[test]
    fn opti_zip_is_picked_over_checksums_and_7z() {
        let releases = json!([
            {"prerelease": false, "tag_name": "nightly", "assets": [
                {"name": "OptiScaler_v10.0.0-pre1_20260908.7z", "browser_download_url": "https://x/n.7z"}
            ]},
            {"prerelease": false, "tag_name": "v0.7.1-hybrid", "assets": [
                {"name": "ASSET-SHA256SUMS-v0.7.1.txt", "browser_download_url": "https://x/sums.txt"},
                {"name": "OptiScaler-DLSSNR-v0.7.1-hybrid.zip", "browser_download_url": "https://x/good.zip"}
            ]}
        ]);
        assert_eq!(
            pick_opti_zip(releases.as_array().unwrap(), false).as_deref(),
            Some("https://x/good.zip")
        );
        // A single zip serves either tick state.
        assert_eq!(
            pick_opti_zip(releases.as_array().unwrap(), true).as_deref(),
            Some("https://x/good.zip")
        );
    }

    /// v0.8.3 of the pre-SR fork ships a standard zip and an -rtx40-mfg one,
    /// MFG listed first. The tick picks; nobody gets the unlock build by
    /// accident of asset order.
    #[test]
    fn opti_mfg_variant_follows_the_tick() {
        let releases = json!([
            {"prerelease": false, "tag_name": "v0.8.3", "assets": [
                {"name": "OptiScaler-NR-v0.8.3-rtx40-mfg.zip", "browser_download_url": "https://x/mfg.zip"},
                {"name": "OptiScaler-NR-v0.8.3-rtx40-mfg.zip.sha256", "browser_download_url": "https://x/mfg.sha"},
                {"name": "OptiScaler-NR-v0.8.3.zip", "browser_download_url": "https://x/std.zip"},
                {"name": "OptiScaler-NR-v0.8.3.zip.sha256", "browser_download_url": "https://x/std.sha"}
            ]}
        ]);
        let r = releases.as_array().unwrap();
        assert_eq!(
            pick_opti_zip(r, false).as_deref(),
            Some("https://x/std.zip")
        );
        assert_eq!(pick_opti_zip(r, true).as_deref(), Some("https://x/mfg.zip"));
    }

    /// The engine choice decides which fork is fetched, and nothing else.
    #[test]
    fn opti_source_selects_the_fork() {
        std::env::remove_var(OPTI_SOURCE_ENV);
        assert_eq!(opti_repo(), OPTI_REPO);
        std::env::set_var(OPTI_SOURCE_ENV, "presr");
        assert_eq!(opti_repo(), OPTI_PRESR_REPO);
        std::env::set_var(OPTI_SOURCE_ENV, "something else");
        assert_eq!(opti_repo(), OPTI_REPO);
        std::env::remove_var(OPTI_SOURCE_ENV);
    }

    /// Dying Light refuses any reduced work resolution: identical failure at
    /// 90% and 85%, fine at 100%. Re-running Install used to write the preset's
    /// smaller value straight back and break the game again (#74).
    #[test]
    fn a_game_that_refused_a_smaller_work_resolution_keeps_full_size() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let mut q = quality_preset::fallback_medium();
        q.work_resolution = 85;
        q.work_upscale = 1;

        // No log yet: the preset is written as chosen.
        write_feeder_cfg(d, &q).unwrap();
        let cfg = fs::read_to_string(d.join("dlss5-feed.cfg")).unwrap();
        assert!(cfg.contains("work_resolution=85"), "{cfg}");

        // A log carrying the failure pins it back to full size.
        fs::write(
            d.join("dlss5-feed.log"),
            "[feed] building: 2304x1296 work resolution (90%) -> 2560x1440 backbuffer\n\
             [feed] work-resolution staging SRV failed\n\
             [feed] failure: resource build\n",
        )
        .unwrap();
        assert!(work_resolution_refused(d));
        write_feeder_cfg(d, &q).unwrap();
        let cfg = fs::read_to_string(d.join("dlss5-feed.cfg")).unwrap();
        assert!(cfg.contains("work_resolution=100"), "{cfg}");
        assert!(cfg.contains("work_upscale=0"), "{cfg}");
    }

    /// A machine whose own log carries the driver-fault verdict must not be
    /// handed the same add-on build again. The evidence has to come from the
    /// log, not from a driver number, because the measurement upstream covers
    /// one driver and assumes the rest (#69).
    /// On a 32-bit game the host writes that verdict into host64\ while the
    /// feed's own log sits beside the exe. Reading only one folder missed it,
    /// and the exclusion of 32-bit games on top of that meant GTA IV was handed
    /// the faulting build every single install (#69).
    #[test]
    fn the_driver_verdict_is_found_from_either_side_of_a_32_bit_layout() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path();
        let host = game.join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        let flag = t.path().join("state").join("driver-fault.txt");
        let faulted = |dir: &Path| faulted_in_driver_at(dir, &flag, Some("616.64".to_owned()));
        assert!(!faulted(&host));

        // The host's own log, which is where a 32-bit game records it.
        fs::write(
            host.join("dlss5-feed-host.log"),
            "[host] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted(&host));

        // And the feed's log beside the exe, one level up from the consumer dir.
        let t2 = tempfile::tempdir().unwrap();
        let host2 = t2.path().join(game::HOST_DIR);
        fs::create_dir_all(&host2).unwrap();
        fs::write(
            t2.path().join("dlss5-feed.log"),
            "[feed] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted(&host2));
    }

    /// Remove deletes the game folder's logs, and a fresh game has none yet, so
    /// a verdict read out of a log has to outlive the folder it was read in.
    /// sempie27 was told to Remove and Install, which erased the evidence and
    /// handed him the faulting build again (#69).
    #[test]
    fn the_driver_verdict_outlives_the_folder_it_was_read_in() {
        let t = tempfile::tempdir().unwrap();
        let host = t.path().join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        let flag = t.path().join("state").join("driver-fault.txt");
        let drv = || Some("616.64".to_owned());

        assert!(!faulted_in_driver_at(&host, &flag, drv()));

        fs::write(
            host.join("dlss5-feed-host.log"),
            "[host] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted_in_driver_at(&host, &flag, drv()));
        assert_eq!(fs::read_to_string(&flag).unwrap().trim(), "616.64");

        // Remove empties the folder; the verdict survives it.
        fs::remove_file(host.join("dlss5-feed-host.log")).unwrap();
        assert!(
            faulted_in_driver_at(&host, &flag, drv()),
            "the verdict must outlive the log"
        );

        // A driver update retires it, and the stale flag is dropped.
        assert!(!faulted_in_driver_at(
            &host,
            &flag,
            Some("620.10".to_owned())
        ));
        assert!(!flag.is_file());
    }

    #[test]
    fn a_driver_fault_in_the_log_pins_the_classic_addon() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let flag = t.path().join("state").join("driver-fault.txt");
        let faulted = |dir: &Path| faulted_in_driver_at(dir, &flag, Some("616.64".to_owned()));
        assert!(!faulted(d));

        fs::write(
            d.join("dlss5-feed-host.log"),
            "[host] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail (on 616.64 exactly; anything newer is untested here). The neural \
             evaluate faults inside the driver's own NGX runtime -- an access violation in \
             D3D12Core.dll, reached through nvngx_dlssnr.dll\n",
        )
        .unwrap();
        assert!(faulted(d));

        // The feed's own log carries the same verdict on a 64-bit game.
        let d2 = tempfile::tempdir().unwrap();
        fs::write(
            d2.path().join("dlss5-feed.log"),
            "[feed] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted(d2.path()));

        // A healthy log changes nothing.
        let d3 = tempfile::tempdir().unwrap();
        fs::write(d3.path().join("dlss5-feed.log"), "[feed] feature ready\n").unwrap();
        let fresh = t.path().join("state2").join("driver-fault.txt");
        assert!(!faulted_in_driver_at(
            d3.path(),
            &fresh,
            Some("616.64".to_owned())
        ));
    }

    /// Two neural consumers in one folder is not two implementations to choose
    /// from: both detour the same NGX entry points and create feature 18 on the
    /// same device, the second create is refused (0xBAD0000B), and the user gets
    /// neither. Installing once with the default and again with Neural Upstream
    /// left exactly that (#75).
    #[test]
    fn the_upstream_route_removes_the_addon_it_replaces() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        fs::write(t.path().join(game::DLSS_DLL), b"x").unwrap(); // native DLSS
        let addon = t.path().join(game::DLSS5_ADDON);
        fs::write(&addon, b"addon").unwrap();
        let st = game::inspect(&exe).unwrap();

        // The step is in the plan for that route, and not for the default one.
        let named = |v: Vec<Step>| -> Vec<&'static str> { v.iter().map(|s| s.name).collect() };
        let with = named(plan_with(&st, Engine::ReShade, false, true));
        let without = named(plan_with(&st, Engine::ReShade, false, false));
        assert!(
            with.iter().any(|n| n.contains("Remove the RenoDX")),
            "{with:?}"
        );
        assert!(
            !without.iter().any(|n| n.contains("Remove the RenoDX")),
            "{without:?}"
        );

        // And it takes the file out.
        let c = reqwest::blocking::Client::new();
        step_dlss5_cleanup(&c, &st, t.path(), &|_, _| {}).unwrap();
        assert!(!addon.exists());
        // A second run is a no-op rather than an error.
        step_dlss5_cleanup(&c, &st, t.path(), &|_, _| {}).unwrap();
    }

    /// Feeder 0.15.0 fixed the sRGB staging-view bug that made every reduced
    /// work resolution fail. A failure logged by an older build must stop
    /// holding the setting down, or the fix never reaches anyone who hit it
    /// (DLSS5-Feeder#85).
    #[test]
    fn the_work_resolution_hold_expires_with_the_feeder_that_logged_it() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let log = |ver: &str| {
            fs::write(
                d.join("dlss5-feed.log"),
                format!(
                    "02:08:22.172  dlss5-feed {ver} (built Sep  7 2026 07:47:43) attached.\n\
                     [feed] work-resolution staging SRV failed\n"
                ),
            )
            .unwrap();
        };

        log("0.14.0-beta.5");
        assert!(
            !work_resolution_refused(d),
            "an old build's failure is stale"
        );
        log("0.15.0");
        assert!(
            work_resolution_refused(d),
            "the fixed build still failing counts"
        );
        log("0.16.2");
        assert!(work_resolution_refused(d));

        // A log with no version line at all is still taken at its word.
        fs::write(
            d.join("dlss5-feed.log"),
            "[feed] work-resolution staging SRV failed\n",
        )
        .unwrap();
        assert!(work_resolution_refused(d));
    }

    /// The 32-bit halves talk a versioned protocol; if one is refreshed and the
    /// other is not, the host exits at startup and nothing says why from inside
    /// the game. Same size is not the same release (#69).
    #[test]
    fn both_thirty_two_bit_halves_carry_the_tag_they_came_from() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let host = d.join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();

        // What an install writes.
        fs::write(d.join(game::FEEDER_MARKER), b"v0.15.1").unwrap();
        fs::write(host.join(game::FEEDER_MARKER), b"v0.15.1").unwrap();
        let agree = |tag: &str| {
            let says = |dir: &Path| {
                fs::read_to_string(dir.join(game::FEEDER_MARKER)).is_ok_and(|m| m.trim() == tag)
            };
            says(d) && says(&host)
        };
        assert!(agree("v0.15.1"));

        // A newer release: neither half is current, so both are replaced.
        assert!(!agree("v0.16.0"));

        // The failure this fixes: the helper refreshed, the in-game half not.
        fs::write(host.join(game::FEEDER_MARKER), b"v0.16.0").unwrap();
        assert!(
            !agree("v0.16.0"),
            "a half-updated pair must not look current"
        );
    }

    /// RTX 40 MFG is one ini key and no extra files, so it can be offered as
    /// part of an install. The Ampere/Turing key in the same section sideloads
    /// a DLL with no published release and is deliberately never written (#83).
    /// Remove left dgVoodoo's d3d9.dll in every DX9 game it had been installed
    /// into, so a game that would not start still would not start afterwards,
    /// and nothing said which file to delete (#91). Only a copy this tool
    /// downloaded goes, and the conf only when this tool created it.
    #[test]
    fn removing_takes_our_dgvoodoo_out_and_leaves_a_user_s_alone() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);

        // Ours, conf included.
        fs::write(d.join("d3d9.dll"), b"dgVoodoo").unwrap();
        fs::write(d.join(game::DGVOODOO_CONF), b"[General]\n").unwrap();
        fs::write(
            d.join(game::DGVOODOO_MARKER),
            format!("{DGVOODOO_TAG}\nconf-ours\n"),
        )
        .unwrap();
        uninstall(&exe).unwrap();
        assert!(!d.join("d3d9.dll").exists());
        assert!(!d.join(game::DGVOODOO_CONF).exists());
        assert!(!d.join(game::DGVOODOO_MARKER).exists());

        // Ours, but the conf was the user's before we merged into it.
        fs::write(d.join("d3d9.dll"), b"dgVoodoo").unwrap();
        fs::write(d.join(game::DGVOODOO_CONF), b"[General]\n").unwrap();
        fs::write(d.join(game::DGVOODOO_MARKER), format!("{DGVOODOO_TAG}\n")).unwrap();
        uninstall(&exe).unwrap();
        assert!(!d.join("d3d9.dll").exists());
        assert!(d.join(game::DGVOODOO_CONF).is_file(), "their conf stays");

        // Someone else's d3d9.dll, no marker: untouched.
        fs::write(d.join("d3d9.dll"), b"theirs").unwrap();
        uninstall(&exe).unwrap();
        assert_eq!(fs::read(d.join("d3d9.dll")).unwrap(), b"theirs");
    }

    /// The MFG add-on validates the frame-generation provider by build and
    /// refuses anything else, so ours goes in and the game's own is kept as
    /// .original — Remove has to put that back, not delete it (#90).
    #[test]
    fn removing_the_mfg_provider_restores_the_game_s_own() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        fs::write(d.join(game::DLSSG_DLL), b"ours").unwrap();
        fs::write(d.join(game::DLSSG_BACKUP), b"the game's").unwrap();
        fs::write(d.join(game::DLSSG_MARKER), b"dlssg-310.9.1").unwrap();

        uninstall(&exe).unwrap();
        assert_eq!(
            fs::read(d.join(game::DLSSG_DLL)).unwrap(),
            b"the game's",
            "the game's provider must come back"
        );
        assert!(!d.join(game::DLSSG_BACKUP).exists());
        assert!(!d.join(game::DLSSG_MARKER).exists());

        // With no backup, ours is simply removed.
        fs::write(d.join(game::DLSSG_DLL), b"ours").unwrap();
        fs::write(d.join(game::DLSSG_MARKER), b"dlssg-310.9.1").unwrap();
        uninstall(&exe).unwrap();
        assert!(!d.join(game::DLSSG_DLL).exists());
    }

    /// The OptiScaler route writes an ini key; the ReShade route needs the
    /// separate add-on, because the fork's built-in unlock reported "DLSSG not
    /// patched: capability not matched" on the reporter's machine (#83). The
    /// add-on is an .addon64, so a 32-bit game never gets it.
    #[test]
    fn mfg_addon_is_planned_on_the_reshade_route_only() {
        let st = game::stub_status(game::Mode::Native, game::Api::Dx12);
        let named = |v: &[Step]| -> Vec<&'static str> { v.iter().map(|s| s.name).collect() };

        std::env::remove_var(ADA_MFG_ENV);
        assert!(!named(&plan_with(&st, Engine::ReShade, false, false)).contains(&STEP_MFG.name));

        std::env::set_var(ADA_MFG_ENV, "1");
        let reshade = named(&plan_with(&st, Engine::ReShade, false, false));
        assert!(reshade.contains(&STEP_MFG.name), "{reshade:?}");
        // Ahead of ReShade config, which writes the add-on list.
        let mfg = reshade.iter().position(|n| *n == STEP_MFG.name).unwrap();
        let cfg = reshade.iter().position(|n| *n == STEP_CONFIG.name).unwrap();
        assert!(mfg < cfg, "{reshade:?}");
        // The OptiScaler route has its own ini key and must not fetch it.
        assert!(!named(&plan_with(&st, Engine::Opti, false, false)).contains(&STEP_MFG.name));
        std::env::remove_var(ADA_MFG_ENV);
    }

    #[test]
    fn ada_mfg_is_written_and_ampere_is_left_alone() {
        std::env::remove_var(ADA_MFG_ENV);
        assert_eq!(ada_mfg(), "false");
        std::env::set_var(ADA_MFG_ENV, "1");
        assert_eq!(ada_mfg(), "true");
        std::env::remove_var(ADA_MFG_ENV);

        // The key lives under [DLSSG] in the shipped ini (v0.7.7 and the
        // v0.8.3 -rtx40-mfg package alike); [FrameGen] holds Enabled/FGInput/
        // FGOutput. Writing it under [FrameGen] was a silent no-op for three
        // releases.
        let ini =
            "[FrameGen]\nEnabled=auto\n\n[DLSSG]\nAdaMfgUnlock=false\nAmpereMfgUnlock=false\n";
        let out = set_ini_key(ini, "DLSSG", "AdaMfgUnlock", "true").unwrap();
        assert!(out.contains("[DLSSG]\nAdaMfgUnlock=true"), "{out}");
        assert!(
            !out.contains("[FrameGen]\nEnabled=auto\nAdaMfgUnlock"),
            "{out}"
        );
        // The two are mutually exclusive upstream: "Never combine with
        // AdaMfgUnlock or an external MFG unlocker."
        assert!(out.contains("AmpereMfgUnlock=false"), "{out}");
    }

    /// OptiScaler's own FSR 3.1 frame generation is four ini keys on files the
    /// package already ships. Off means the ini is not touched, because the
    /// overlay's own frame-generation choice lives there too.
    #[test]
    fn opti_fg_is_four_keys_and_off_is_untouched() {
        std::env::remove_var(OPTI_FG_ENV);
        assert!(!opti_fg());
        std::env::set_var(OPTI_FG_ENV, "1");
        assert!(opti_fg());
        std::env::remove_var(OPTI_FG_ENV);

        let ini =
            "[FrameGen]\nEnabled=auto\nFGInput=auto\nFGOutput=auto\n\n[OptiFG]\nHUDFix=auto\n";
        let mut cur = ini.to_owned();
        for (section, key, value) in [
            ("FrameGen", "Enabled", "true"),
            ("FrameGen", "FGInput", "upscaler"),
            ("FrameGen", "FGOutput", "fsrfg"),
            ("OptiFG", "HUDFix", "true"),
        ] {
            cur = set_ini_key(&cur, section, key, value).unwrap();
        }
        assert!(
            cur.contains("[FrameGen]\nEnabled=true\nFGInput=upscaler\nFGOutput=fsrfg"),
            "{cur}"
        );
        assert!(cur.contains("[OptiFG]\nHUDFix=true"), "{cur}");
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
    fn vulkan_feeder_kit_writes_addon_fx_and_note() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let z = t.path().join("feeder.zip");
        write_zip(
            &z,
            &[(game::FEEDER_ADDON, b"addon"), (game::FEEDER_FX, b"fx")],
            &[],
        );
        let out = copy_vulkan_feeder_kit_from_zip(&z, d, "v0.14.0").unwrap();
        assert!(d.join(game::FEEDER_ADDON).is_file());
        assert!(d
            .join("reshade-shaders")
            .join("Shaders")
            .join(game::FEEDER_FX)
            .is_file());
        assert!(d.join("VULKAN-SETUP.txt").is_file());
        assert!(out.iter().any(|s| s.contains("VULKAN-SETUP")));
        assert!(out.iter().any(|s| s.contains("v0.14.0")));
    }

    #[test]
    fn dgvoodoo_from_zip_writes_d3d9_and_conf_ignores_off() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        // Old ReShade rename must not be restored as dgVoodoo.
        fs::write(d.join("d3d9.dll.off"), b"MZ old reshade not dgVoodoo").unwrap();
        let z = d.join("dgVoodoo2_87_3.zip");
        write_zip(
            &z,
            &[(
                "MS/x86/D3D9.dll",
                b"MZ....dgVoodoo2 wrapper bytes for detect....",
            )],
            &[],
        );
        let out = install_dgvoodoo_from_zip(&z, d, 32).unwrap();
        assert!(out.contains(&"d3d9.dll".to_string()));
        assert!(out.contains(&"dgVoodoo.conf".to_string()));
        let dll = fs::read(d.join("d3d9.dll")).unwrap();
        assert!(dll.windows(8).any(|w| w.eq_ignore_ascii_case(b"dgVoodoo")));
        assert_ne!(
            fs::read(d.join("d3d9.dll.off")).unwrap(),
            dll,
            "must not restore d3d9.dll.off"
        );
        let conf = fs::read_to_string(d.join("dgVoodoo.conf")).unwrap();
        assert!(conf.contains("OutputAPI = d3d11_fl11_0"));
        assert!(conf.contains("VRAM = 4096"));
        assert!(conf.contains("Antialiasing = appdriven"));
        assert!(conf.contains("FastVideoMemoryAccess = false"));
        assert!(game::is_dgvoodoo(d));
    }

    #[test]
    fn dgvoodoo_conf_merge_preserves_user_keys_and_floors_vram() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::write(
            d.join("dgVoodoo.conf"),
            "[General]\nOutputAPI = bestavailable\nAdapters = all\n\
             [DirectX]\nVRAM = 256\nFiltering = force16bit\n",
        )
        .unwrap();
        write_dgvoodoo_conf(d).unwrap();
        assert!(d.join("dgVoodoo.conf.bak").is_file());
        let conf = fs::read_to_string(d.join("dgVoodoo.conf")).unwrap();
        assert!(conf.contains("OutputAPI = d3d11_fl11_0"));
        assert!(!conf.to_ascii_lowercase().contains("bestavailable"));
        assert!(conf.contains("VRAM = 4096"));
        assert!(conf.contains("Filtering = force16bit"));
        assert!(conf.contains("Adapters = all"));
        // Second merge must not overwrite bak with already-merged text.
        let bak1 = fs::read(d.join("dgVoodoo.conf.bak")).unwrap();
        write_dgvoodoo_conf(d).unwrap();
        assert_eq!(fs::read(d.join("dgVoodoo.conf.bak")).unwrap(), bak1);
        // Keep a higher user VRAM.
        fs::write(
            d.join("dgVoodoo.conf"),
            "[General]\nOutputAPI = d3d11_fl11_0\n[DirectX]\nVRAM = 8192\n",
        )
        .unwrap();
        write_dgvoodoo_conf(d).unwrap();
        let conf2 = fs::read_to_string(d.join("dgVoodoo.conf")).unwrap();
        assert!(conf2.contains("VRAM = 8192"));
    }

    #[test]
    fn dgvoodoo_from_zip_picks_x64_member() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let z = d.join("dgVoodoo2_87_3.zip");
        write_zip(
            &z,
            &[(
                "MS/x64/D3D9.dll",
                b"MZ....dgVoodoo2 wrapper bytes for detect....",
            )],
            &[],
        );
        install_dgvoodoo_from_zip(&z, d, 64).unwrap();
        assert!(game::is_dgvoodoo(d));
    }

    #[test]
    fn plan_puts_dgvoodoo_first_for_dx9() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe_with_imports(&t.path().join("g3.exe"), game::PE_X86, &["engine.dll"]);
        make_pe_with_imports(
            &t.path().join("Engine.dll"),
            game::PE_X86,
            &["d3d9.dll", "kernel32.dll"],
        );
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = game::inspect(&exe).unwrap();
        assert!(st.needs_dgvoodoo());
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[0], "dgVoodoo 2.87.3 (DX9 → D3D11)");
        assert!(names.iter().any(|n| n.starts_with("ReShade")));
    }

    #[test]
    fn plan_refreshes_dgvoodoo_conf_when_already_present() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe_with_imports(&d.join("g3.exe"), game::PE_X86, &["engine.dll"]);
        make_pe_with_imports(
            &d.join("Engine.dll"),
            game::PE_X86,
            &["d3d9.dll", "kernel32.dll"],
        );
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        fs::write(
            d.join("dgVoodoo.conf"),
            b"[General]\nOutputAPI = bestavailable\n",
        )
        .unwrap();
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = game::inspect(&exe).unwrap();
        assert!(!st.needs_dgvoodoo());
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[0], "dgVoodoo 2.87.3 (DX9 → D3D11)");
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
        assert!(
            installed.len() >= 4,
            "expected at least Kernel/TRAA/include/png, got {installed:?}"
        );
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
    fn traa_ui_protect_patch_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let shaders = t.path().join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&shaders).unwrap();
        // Anchors must match stock lumenite_TRAA.fx (LumeniteFX mainline).
        let body = concat!(
            "uniform int EDGE_MODE <\n",
            "    ui_tooltip = \"Luma: shading and texture edges as well; the classic DLAA mask.\\n\"\n",
            "                 \"Geometric: silhouettes only, ignores flat UI.\";\n",
            "    > = 0;\n",
            "/*--------------.\n",
            "| :: IMPORTS :: |\n",
            "'--------------*/\n",
            "namespace Kernel {}\n",
            "namespace LumeniteTRAA {\n",
            "    confidence = saturate(confidence + 0.11 * 4.0 * confidence * (1.0 - confidence));\n",
            "\n",
            "    float2 historyUV = texcoord + flow;\n",
            "technique Lumenite_TRAA <\n",
            "    ui_tooltip = \"Temporal Reprojection Anti-Aliasing.\";\n",
            ">\n",
            "}\n",
        );
        let dest = shaders.join("lumenite_TRAA.fx");
        fs::write(&dest, body).unwrap();
        let first = apply_traa_ui_patch(t.path()).unwrap().unwrap();
        assert!(first.contains("UI protect patch"), "{first}");
        let text = fs::read_to_string(&dest).unwrap();
        assert!(text.contains("DLSS5_TRAA_UI_PROTECT"));
        assert!(text.contains("UI_PROTECT"));
        assert!(text.contains("> = 1;"));
        let second = apply_traa_ui_patch(t.path()).unwrap().unwrap();
        assert!(second.contains("already applied"), "{second}");
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
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
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
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, true, false)
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
        let names: Vec<&str> = plan_with(&st, Engine::Opti, true, false)
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
    fn set_ini_key_is_section_scoped_and_appends() {
        // The RE Engine hotfixes go under [Hotfix]; the same key names exist
        // elsewhere in OptiScaler.ini, so only that section may move (#44).
        let ini = "[Menu]
ManualInputPolling=auto

[Hotfix]
ManualInputPolling=auto
ExtendedStateRestore=true
";
        let out = set_ini_key(ini, "Hotfix", "ManualInputPolling", "true").unwrap();
        assert_eq!(
            out,
            "[Menu]
ManualInputPolling=auto

[Hotfix]
ManualInputPolling=true
ExtendedStateRestore=true
"
        );
        // A value that already reads that way is left alone.
        assert!(set_ini_key(&out, "Hotfix", "ManualInputPolling", "true").is_none());
        // Turning one back off is the same operation.
        let off = set_ini_key(&out, "Hotfix", "ExtendedStateRestore", "false").unwrap();
        assert!(off.contains("ExtendedStateRestore=false"));
        // Missing section is appended rather than dropped.
        let added = set_ini_key(
            "[Menu]
X=1
",
            "Hotfix",
            "RestoreComputeSignature",
            "true",
        )
        .unwrap();
        assert!(added.ends_with(
            "
[Hotfix]
RestoreComputeSignature=true
"
        ));
    }

    /// The model-resolution dial is the biggest performance lever on the
    /// OptiScaler route: cost falls with the square of WorkingScale.
    #[test]
    fn working_scale_is_written_and_bounded() {
        std::env::remove_var(WORKING_SCALE_ENV);
        assert_eq!(working_scale(), "1.0");
        std::env::set_var(WORKING_SCALE_ENV, "0.75");
        assert_eq!(working_scale(), "0.75");
        // Nonsense and out-of-range values fall back rather than reaching the ini.
        std::env::set_var(WORKING_SCALE_ENV, "banana");
        assert_eq!(working_scale(), "1.0");
        std::env::set_var(WORKING_SCALE_ENV, "9");
        assert_eq!(working_scale(), "1.0");
        std::env::remove_var(WORKING_SCALE_ENV);

        let ini = "[DlssNr]\nEnabled=auto\n";
        let out = set_ini_key(ini, "DlssNr", "WorkingScale", "0.75").unwrap();
        assert!(out.contains("WorkingScale=0.75"), "{out}");
    }

    #[test]
    fn dlss_nr_enabled_is_section_scoped() {
        // "Enabled" also lives under other headings; only DlssNr's may move.
        let ini = "[OptiFG]\nEnabled=auto\n\n[DlssNr]\n; comment\nEnabled=auto\n";
        assert_eq!(
            set_dlss_nr_enabled(ini).unwrap(),
            "[OptiFG]\nEnabled=auto\n\n[DlssNr]\n; comment\nEnabled=true\n"
        );
        assert!(set_dlss_nr_enabled("[DlssNr]\nEnabled=true\n").is_none());
        // No section at all: append one.
        assert_eq!(
            set_dlss_nr_enabled("[OptiFG]\nEnabled=auto\n").unwrap(),
            "[OptiFG]\nEnabled=auto\n\n[DlssNr]\nEnabled=true\n"
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
            opti_presr: Some("v0.7.7".into()),
            dlss: Some("dlss-310.9.0".into()),
            dlssnr: Some("dlssnr-310.8.SF-v2".into()),
            aio: None,
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

    /// The two builds number their releases independently, so the pre-SR fork's
    /// v0.7.7 was compared against the stable build's v0.2.0-dlssnr and reported
    /// an update on every single run, which Install could never clear (#88).
    #[test]
    fn the_presr_fork_is_compared_against_its_own_releases() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let latest = Latest {
            reshade: None,
            feeder: None,
            opti: Some("v0.2.0-dlssnr".into()),
            opti_presr: Some("v0.7.7".into()),
            dlss: None,
            dlssnr: None,
            aio: None,
        };

        // Current pre-SR install: the repo line settles it.
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.7.7\n# repo wilsjo2/OptiScaler-DLSSNR-PreSR-Multipass\ndxgi.dll\n",
        )
        .unwrap();
        assert!(stale_components(d, &latest).is_empty());

        // Behind on the pre-SR fork: named against that fork's newest.
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.7.6\n# repo wilsjo2/OptiScaler-DLSSNR-PreSR-Multipass\ndxgi.dll\n",
        )
        .unwrap();
        assert_eq!(
            stale_components(d, &latest),
            vec!["OptiScaler v0.7.6 → v0.7.7".to_string()]
        );

        // A manifest from before the repo line: the stable build's tags all end
        // in "-dlssnr", so the shape of the tag says which build it is.
        fs::write(d.join(game::OPTI_MANIFEST), "# tag v0.7.7\ndxgi.dll\n").unwrap();
        assert!(stale_components(d, &latest).is_empty());
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.2.0-dlssnr\ndxgi.dll\n",
        )
        .unwrap();
        assert!(stale_components(d, &latest).is_empty());
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
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names.len(), 7); // + the GPU-preference step
        assert_eq!(names[2], "DLSS5-Feeder");
        st.mode = game::Mode::Native;
        st.api = game::Api::Dx12;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
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
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[2], "DLSS 5 DX11 bridge");
    }

    #[test]
    fn uninstall_all_removes_reshade_even_with_leftover_shaders() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        fs::write(d.join(game::RESHADE_MARKER), b"6.8.0").unwrap();
        let sh = d.join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&sh).unwrap();
        fs::write(d.join(game::FEEDER_ADDON), b"x").unwrap();
        fs::write(sh.join(game::FEEDER_FX), b"x").unwrap();
        fs::write(sh.join("ReShade.fxh"), b"x").unwrap();
        fs::write(d.join("ReShade.ini"), b"x").unwrap();
        fs::write(d.join("ReShadePreset.ini"), b"x").unwrap();
        fs::write(d.join("dlss5-feed.cfg"), b"x").unwrap();
        // Pre-existing shader pack (Gothic 3 etc.) must not block dxgi.dll removal.
        fs::write(sh.join("Clarity.fx"), b"user shader").unwrap();
        // dgVoodoo for DX9 games must never be touched.
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        fs::write(
            d.join("dgVoodoo.conf"),
            b"[DirectX]\nOutputAPI = bestavailable\n",
        )
        .unwrap();

        let (removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_none(), "{kept:?}");
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join(game::RESHADE_MARKER).exists());
        assert!(!d.join("ReShade.ini").exists());
        assert!(!d.join("dlss5-feed.cfg").exists());
        assert!(!d.join(game::FEEDER_ADDON).is_file());
        assert!(d
            .join("reshade-shaders")
            .join("Shaders")
            .join("Clarity.fx")
            .is_file());
        assert!(d.join("d3d9.dll").is_file(), "dgVoodoo d3d9.dll must stay");
        assert!(d.join("dgVoodoo.conf").is_file());
        assert!(!game::inspect(&exe).unwrap().reshade);
    }

    #[test]
    fn uninstall_all_cleans_empty_reshade_shaders_tree() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        let sh = d.join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&sh).unwrap();
        fs::write(sh.join("ReShade.fxh"), b"x").unwrap();
        fs::write(d.join("ReShade.ini"), b"x").unwrap();
        let (removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_none(), "{kept:?}");
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join("ReShade.ini").exists());
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

    /// Neural Upstream is the neural consumer itself, so it takes the RenoDX
    /// add-on's place in the plan rather than being added next to it, and it
    /// needs the model beside it (#50).
    #[test]
    fn upstream_plan_replaces_the_renodx_consumer() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let mut st = game::inspect(&exe).unwrap();
        st.mode = game::Mode::Native;
        let stable: Vec<&str> = plan_with(&st, Engine::ReShade, false, false)
            .iter()
            .map(|s| s.name)
            .collect();
        let upstream: Vec<&str> = plan_with(&st, Engine::ReShade, false, true)
            .iter()
            .map(|s| s.name)
            .collect();
        assert!(stable.contains(&"DLSS 5 add-on + models"));
        assert!(!stable.contains(&"Neural Upstream add-on (experimental)"));
        assert!(upstream.contains(&"Neural Upstream add-on (experimental)"));
        assert!(upstream.contains(&"DLSS 5 model (nvngx_dlssnr.dll)"));
        assert!(!upstream.contains(&"DLSS 5 add-on + models"));
    }

    /// It reads the colour buffer the game hands its own DLSS, so a game
    /// without DLSS cannot feed it: refuse by name instead of installing.
    #[test]
    fn upstream_refuses_a_game_with_no_dlss_of_its_own() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let e = run_all_with(
            &exe,
            Engine::ReShade,
            false,
            true,
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("needs a game with DLSS of its own"),
            "{e:#}"
        );
    }

    #[test]
    fn opti_plan_and_engine_gate() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let st = game::inspect(&exe).unwrap();
        let names: Vec<&str> = plan_with(&st, Engine::Opti, false, false)
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
        let err = run_all_with(
            &exe,
            Engine::Opti,
            false,
            false,
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
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
        let err = run_all_with(
            &exe,
            Engine::Opti,
            false,
            false,
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("64-bit only"));
    }

    /// The AIO is the whole consumer: no Feeder, no RenoDX add-on; the model
    /// and NVIDIA's two runtimes beside it. Switching back
    /// to the ReShade route removes it first, so ReShade never loads two
    /// neural consumers on one frame.
    #[test]
    fn aio_route_is_reshade_plus_the_addon_and_runtimes_only() {
        let named = |v: &[Step]| -> Vec<&'static str> { v.iter().map(|s| s.name).collect() };
        let st = game::stub_status(game::Mode::Feeder, game::Api::Dx12);
        let aio = named(&plan_with(&st, Engine::Aio, false, false));
        assert_eq!(
            aio,
            vec![
                STEP_RESHADE.name,
                STEP_AIO.name,
                STEP_DLSSNR_ONLY.name,
                STEP_AIO_RUNTIME.name,
                STEP_AIO_CONFIG.name,
                STEP_GPU_PREF.name,
            ]
        );
        let mut st = game::stub_status(game::Mode::Feeder, game::Api::Dx12);
        assert!(
            !named(&plan_with(&st, Engine::ReShade, false, false)).contains(&STEP_AIO_CLEANUP.name)
        );
        st.aio = true;
        let back = named(&plan_with(&st, Engine::ReShade, false, false));
        assert_eq!(back[1], STEP_AIO_CLEANUP.name, "{back:?}");
    }

    #[test]
    fn uninstall_removes_aio_manifest_files_and_nothing_else() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        let shaders = d.join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&shaders).unwrap();
        fs::create_dir_all(d.join("licenses")).unwrap();
        fs::write(d.join(game::AIO_ADDON), b"a").unwrap();
        fs::write(d.join("nvngx.dll"), b"bridge").unwrap();
        fs::write(shaders.join("DLSS5_AIO_Feed.fx"), b"fx").unwrap();
        fs::write(shaders.join("Mine.fx"), b"keep").unwrap();
        fs::write(d.join("licenses").join("NOTICE.txt"), b"n").unwrap();
        fs::write(
            d.join(game::AIO_MANIFEST),
            format!(
                "# tag v2.2.4\n# repo {AIO_REPO}\n{}\nnvngx.dll\nreshade-shaders/Shaders/DLSS5_AIO_Feed.fx\nlicenses/NOTICE.txt",
                game::AIO_ADDON
            ),
        )
        .unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.iter().any(|r| r == game::AIO_ADDON), "{removed:?}");
        assert!(!d.join(game::AIO_ADDON).exists());
        assert!(!d.join("nvngx.dll").exists());
        assert!(!shaders.join("DLSS5_AIO_Feed.fx").exists());
        assert!(shaders.join("Mine.fx").exists());
        assert!(!d.join("licenses").exists());
        assert!(!d.join(game::AIO_MANIFEST).exists());
        // With the add-on gone, uninstall_all sees no foreign add-on either.
        let (_, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_none(), "{kept:?}");
    }

    #[test]
    fn run_all_refuses_aio_on_32bit_before_network() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let err = run_all_with(
            &exe,
            Engine::Aio,
            false,
            false,
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("64-bit only"));
    }
}
