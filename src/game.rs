//! Game-folder inspection: exe bitness, ReShade presence, installed pieces.

use crate::gpu;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const PE_X64: u16 = 0x8664;
pub const PE_X86: u16 = 0x014C;

pub const FEEDER_ADDON: &str = "dlss5-feed.addon64";
/// 32-bit games: the in-game half of the Feeder, and the 64-bit helper folder
/// beside the exe that holds everything a 32-bit process cannot load
/// (a 64-bit ReShade, the DLSS 5 add-on, the two NVIDIA DLLs, the helper exe).
pub const FEEDER_ADDON32: &str = "dlss5-feed.addon32";
pub const HOST_DIR: &str = "host64";
pub const HOST_EXE: &str = "dlss5-feed-host64.exe";
pub const FEEDER_FX: &str = "DLSS5_Feed.fx";
pub const DLSS5_ADDON: &str = "renodx-dlss5.addon64";
pub const DLSSNR_DLL: &str = "nvngx_dlssnr.dll";
pub const DLSS_DLL: &str = "nvngx_dlss.dll";
pub const LUMENITE_KERNEL_FX: &str = "lumenite_Kernel.fx";
pub const LUMENITE_BLUENOISE: &str = "lumenite_bluenoise256.png";
pub const BRIDGE_ADDON: &str = "dlss5-bridge.addon64";
/// Files this tool wrote for an OptiScaler install, one path per line.
pub const OPTI_MANIFEST: &str = ".dlss5oneclick-optiscaler-manifest";
/// Sidecar written next to an `nvngx_dlss.dll` this tool placed, so it is never mistaken for the game's own.
pub const DLSS_MARKER: &str = "nvngx_dlss.dll.dlss5oneclick";
/// Same idea for the neural model and for ReShade: the release tag / version this
/// tool placed, so Install can tell "ours and stale" from "the user's own".
pub const DLSSNR_MARKER: &str = "nvngx_dlssnr.dll.dlss5oneclick";
pub const RESHADE_MARKER: &str = "dxgi.dll.dlss5oneclick";
/// The DLSS5-Feeder release tag this tool placed, so a stale install can be
/// spotted without downloading the zip to compare sizes.
pub const FEEDER_MARKER: &str = "dlss5-feed.dlss5oneclick";
pub const RESHADE_PROXY: &str = "dxgi.dll";
/// Shader headers the official installer fetches from crosire/reshade-shaders (branch `slim`).
/// Not inside the setup exe. DLSS5_Feed.fx and every lumenite_*.fx include ReShade.fxh;
/// DLSS5_Feed.fx also includes DrawText.fxh; ReShadeUI.fxh is the standard companion.
pub const RESHADE_HEADERS: [&str; 3] = ["ReShade.fxh", "ReShadeUI.fxh", "DrawText.fxh"];
/// Name of the RenoDX game mod this tool placed (one line), so Remove takes only that one.
pub const RENODX_MANIFEST: &str = ".dlss5oneclick-renodx";
/// REFramework (praydog) loads as dinput8.dll; RE Engine games crash under ReShade without it.
pub const REFRAMEWORK_DLL: &str = "dinput8.dll";
pub const REFRAMEWORK_MARKER: &str = ".dlss5oneclick-reframework";
/// Every RE Engine game keeps its base archive under this name next to the exe.
pub const RE_ENGINE_PAK: &str = "re_chunk_000.pak";
pub const RESHADE_INI: &str = "ReShade.ini";
pub const RESHADE_PRESET: &str = "ReShadePreset.ini";

/// 64 or 32, read from the PE header's Machine field.
pub fn exe_bitness(exe: &Path) -> Result<u8> {
    let mut f = fs::File::open(exe).with_context(|| format!("cannot open {}", exe.display()))?;
    let mut head = [0u8; 0x40];
    f.read_exact(&mut head)
        .with_context(|| format!("{} is not a Windows executable", exe.display()))?;
    if &head[..2] != b"MZ" {
        bail!("{} is not a Windows executable", exe.display());
    }
    let pe_off = u32::from_le_bytes([head[0x3C], head[0x3D], head[0x3E], head[0x3F]]);
    f.seek(SeekFrom::Start(pe_off as u64))?;
    let mut sig = [0u8; 6];
    f.read_exact(&mut sig)
        .with_context(|| format!("{} has no PE header", exe.display()))?;
    if &sig[..4] != b"PE\0\0" {
        bail!("{} has no PE header", exe.display());
    }
    match u16::from_le_bytes([sig[4], sig[5]]) {
        PE_X64 => Ok(64),
        PE_X86 => Ok(32),
        m => bail!("{}: unsupported machine type 0x{m:04x}", exe.display()),
    }
}

/// A ReShade proxy DLL carries a literal "ReShade" string and is >1 MB.
pub fn is_reshade_dll(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() < (1 << 20) {
        return false;
    }
    match fs::read(path) {
        Ok(bytes) => is_reshade_image(&bytes),
        Err(_) => false,
    }
}

/// OptiScaler's own `dxgi.dll` carries the string `ReShade` six times, because
/// it can load ReShade itself — so "contains ReShade" called it ReShade and
/// refused to update a game that had OptiScaler installed. crosire's name is in
/// every real ReShade build (36 hits of `ReShade`, 3 of `crosire` in 6.8.0)
/// and in none of OptiScaler's, so it settles the two apart; a build that
/// somehow lacks it still counts unless the file says OptiScaler.
fn is_reshade_image(bytes: &[u8]) -> bool {
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    has(b"ReShade") && (has(b"crosire") || !has(b"OptiScaler"))
}

/// Which install path applies to a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Game has no DLSS: DLSS5-Feeder + LumeniteFX fake the DLSS contract.
    Feeder,
    /// Game ships its own DLSS: the DLSS 5 add-on hooks the game's NGX calls directly
    /// (plus dlss5-dx11-bridge when the game renders with D3D11).
    Native,
}

/// Graphics API the exe imports, from its PE import table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// Direct3D 10/10.1. The 32-bit Feeder add-on runs these natively from
    /// 0.13.1-beta.1 (a private D3D11 relay device inside the game process);
    /// a game imports `d3d10_1.dll` rather than `d3d10.dll` in practice.
    Dx10,
    Dx11,
    Dx12,
    /// Imports `vulkan-1.dll` and no Direct3D. ReShade reaches a Vulkan game
    /// through a registered Vulkan layer, not through a `dxgi.dll` beside the
    /// exe, so this install has nothing to load (#6, Detroit: Become Human).
    Vulkan,
    /// Neither d3d11.dll nor d3d12.dll is a static import (loaded at runtime, or DX9/Vulkan).
    Unknown,
}

impl Api {
    pub fn label(self) -> &'static str {
        match self {
            Api::Dx10 => "DX10",
            Api::Dx11 => "DX11",
            Api::Dx12 => "DX12",
            Api::Vulkan => "Vulkan",
            Api::Unknown => "API unknown, assuming DX12",
        }
    }
}

/// Lower-cased DLL names from the exe's static import table. Empty on any parse problem.
pub fn pe_imports(exe: &Path) -> Vec<String> {
    let Ok(data) = fs::read(exe) else {
        return vec![];
    };
    let rd32 = |o: usize| -> Option<u32> {
        data.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let rd16 =
        |o: usize| -> Option<u16> { data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };
    let parse = || -> Option<Vec<String>> {
        if data.get(..2)? != b"MZ" {
            return None;
        }
        let pe = rd32(0x3C)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let nsec = rd16(coff + 2)? as usize;
        let opt_size = rd16(coff + 16)? as usize;
        let opt = coff + 20;
        let magic = rd16(opt)?;
        let dd_off = match magic {
            0x20B => 112,
            0x10B => 96,
            _ => return None,
        };
        let import_rva = rd32(opt + dd_off + 8)? as usize;
        if import_rva == 0 {
            return Some(vec![]);
        }
        let sec = opt + opt_size;
        let mut sections = Vec::new();
        for i in 0..nsec {
            let s = sec + i * 40;
            sections.push((
                rd32(s + 12)? as usize,
                rd32(s + 16)? as usize,
                rd32(s + 20)? as usize,
            ));
        }
        let to_off = |rva: usize| -> Option<usize> {
            sections
                .iter()
                .find(|(va, size, _)| rva >= *va && rva < va + size)
                .map(|(va, _, raw)| raw + (rva - va))
        };
        let mut names = Vec::new();
        let mut desc = to_off(import_rva)?;
        for _ in 0..512 {
            let name_rva = rd32(desc + 12)? as usize;
            if name_rva == 0 && rd32(desc)? == 0 {
                break;
            }
            if let Some(off) = to_off(name_rva) {
                let end = data[off..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|n| off + n)
                    .unwrap_or(off);
                names.push(String::from_utf8_lossy(&data[off..end]).to_ascii_lowercase());
            }
            desc += 20;
        }
        Some(names)
    };
    parse().unwrap_or_default()
}

/// Which D3D a static import table implies. `d3d10_1.dll` is what a Direct3D
/// 10 game actually imports; the upstream installer looked only for
/// `d3d10.dll` and mistook such games for DirectX 9.
fn classify_imports(imports: &[String]) -> Api {
    let has = |n: &str| imports.iter().any(|i| i == n);
    if has("d3d12.dll") {
        Api::Dx12
    } else if has("d3d11.dll") {
        Api::Dx11
    } else if has("d3d10_1.dll") || has("d3d10.dll") {
        Api::Dx10
    } else if has("vulkan-1.dll") {
        Api::Vulkan
    } else {
        Api::Unknown
    }
}

/// True when the game carries its own DirectX 12 Agility SDK runtime.
/// Unreal puts it in a `D3D12` subfolder; Unity players declare the exe's own
/// folder, so the file sits directly beside the exe. Both count, and the
/// second layout is the one that produced a 0x887E0003 nobody could find
/// (dlss5-bridge#24).
pub fn has_agility_redist(dir: &Path) -> Option<PathBuf> {
    [
        dir.join("D3D12").join("D3D12Core.dll"),
        dir.join("D3D12Core.dll"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

pub fn detect_api(exe: &Path) -> Api {
    use classify_imports as classify;
    let api = classify(&pe_imports(exe));
    let agility_sdk = exe
        .parent()
        .is_some_and(|d| has_agility_redist(d).is_some());
    if api == Api::Dx12 || (api == Api::Dx11 && agility_sdk) {
        // The DirectX 12 Agility SDK redist ships only with D3D12 renderers;
        // RE Engine exes import d3d11.dll statically and create D3D12 at runtime.
        return Api::Dx12;
    }
    if api != Api::Unknown {
        return api;
    }
    // Engines like Unity and Unreal load D3D from an engine DLL next to the exe
    // (UnityPlayer.dll, *-Win64-Shipping.dll, ...). Scan those, largest first,
    // skipping proxies/add-ons that would mislead (dxgi.dll, d3d*.dll, nvngx*).
    let Some(dir) = exe.parent() else {
        return Api::Unknown;
    };
    let Ok(rd) = fs::read_dir(dir) else {
        return Api::Unknown;
    };
    let mut dlls: Vec<(u64, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")))
        .filter(|p| {
            let n = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            !(n.starts_with("dxgi")
                || n.starts_with("d3d")
                || n.starts_with("nvngx")
                || n.starts_with("reshade"))
        })
        .filter_map(|p| fs::metadata(&p).ok().map(|m| (m.len(), p)))
        .collect();
    dlls.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    let mut seen_dx11 = false;
    for (_, dll) in dlls.into_iter().take(12) {
        match classify(&pe_imports(&dll)) {
            Api::Dx12 => return Api::Dx12,
            Api::Dx11 => seen_dx11 = true,
            Api::Dx10 | Api::Vulkan | Api::Unknown => {}
        }
    }
    if seen_dx11 {
        Api::Dx11
    } else {
        Api::Unknown
    }
}

/// dgVoodoo2 next to the exe: its own config/control panel, or its name inside
/// the wrapper DLL. It translates D3D9 to D3D11, which the Feeder supports.
pub fn is_dgvoodoo(game_dir: &Path) -> bool {
    if game_dir.join("dgVoodoo.conf").is_file() || game_dir.join("dgVoodooCpl.exe").is_file() {
        return true;
    }
    let dll = game_dir.join("d3d9.dll");
    match fs::read(&dll) {
        Ok(b) => b.windows(8).any(|w| w.eq_ignore_ascii_case(b"dgVoodoo")),
        Err(_) => false,
    }
}

/// Anti-cheat present in the install tree, by the files those systems ship.
/// ReShade add-on injection is exactly what they look for: kicks at best, bans
/// at worst. Verified file names: EAC `EasyAntiCheat[_EOS]/EasyAntiCheat_EOS_Setup.exe`,
/// BattlEye `BattlEye/BEService_x64.exe`, `Install_BattlEye.bat`, `*_BE.exe`,
/// GameGuard `tools/GGSetup.exe` or a `GameGuard` folder.
pub fn detect_anticheat(game_dir: &Path) -> Option<&'static str> {
    fn walk(d: &Path, depth: u8) -> Option<&'static str> {
        let rd = fs::read_dir(d).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            let n = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if p.is_dir() {
                if n == "easyanticheat" || n == "easyanticheat_eos" {
                    return Some("Easy Anti-Cheat");
                }
                if n == "battleye" {
                    return Some("BattlEye");
                }
                if n == "gameguard" {
                    return Some("GameGuard");
                }
                if depth > 0 {
                    if let Some(hit) = walk(&p, depth - 1) {
                        return Some(hit);
                    }
                }
            } else {
                if n.starts_with("easyanticheat") && n.ends_with(".exe") {
                    return Some("Easy Anti-Cheat");
                }
                if n == "beservice_x64.exe" || n == "install_battleye.bat" || n.ends_with("_be.exe")
                {
                    return Some("BattlEye");
                }
                if n == "ggsetup.exe" || n == "gameguard.des" {
                    return Some("GameGuard");
                }
            }
        }
        None
    }
    walk(game_dir, 3)
}

/// Online titles whose anti-cheat ships no marker files (Blizzard's, Riot's).
/// Overwatch also blocks unsigned DLLs outright (add-ons fail with 0x80090006).
pub fn known_anticheat_exe(exe: &Path) -> Option<&'static str> {
    let n = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match n.as_str() {
        "overwatch.exe" => Some("Blizzard anti-cheat (Overwatch)"),
        "valorant.exe" | "valorant-win64-shipping.exe" => Some("Riot Vanguard"),
        "leagueclient.exe" | "league of legends.exe" => Some("Riot Vanguard"),
        _ => None,
    }
}

/// True if the game ships its own DLSS.
///
/// Signals, any one is enough: an `nvngx_dlss.dll` under the exe's folder (depth <= 4)
/// that this tool did not place (no sidecar marker next to it), or Streamline /
/// frame-generation / ray-reconstruction runtimes (`sl.*.dll`, `nvngx_dlssg.dll`,
/// `nvngx_dlssd.dll`) which only a DLSS-integrated game ships.
pub fn game_ships_dlss(game_dir: &Path) -> bool {
    fn walk(d: &Path, depth: u8) -> bool {
        let Ok(rd) = fs::read_dir(d) else {
            return false;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                let n = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if n == DLSS_DLL && !p.with_file_name(DLSS_MARKER).is_file() {
                    return true;
                }
                if n == "nvngx_dlssg.dll"
                    || n == "nvngx_dlssd.dll"
                    || (n.starts_with("sl.") && n.ends_with(".dll"))
                {
                    return true;
                }
            } else if depth > 0 && p.is_dir() && walk(&p, depth - 1) {
                return true;
            }
        }
        false
    }
    if walk(game_dir, 4) {
        return true;
    }
    // Unreal: exe in <Project>/Binaries/Win64, DLSS in
    // <Project>/Plugins/NVIDIA/DLSS/Binaries/ThirdParty/Win64 (or Engine/Plugins/...).
    let unreal_root = game_dir
        .parent()
        .filter(|b| {
            b.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("binaries"))
        })
        .and_then(Path::parent);
    match unreal_root {
        Some(proj) => {
            walk(&proj.join("Plugins"), 7)
                || proj
                    .parent()
                    .is_some_and(|root| walk(&root.join("Engine").join("Plugins"), 7))
        }
        None => false,
    }
}

#[derive(Debug, Clone)]
pub struct GameStatus {
    pub mode: Mode,
    pub api: Api,
    pub bridge: bool,
    pub opti: bool,
    pub gpu: Option<(gpu::Gpu, gpu::Tier)>,
    pub exe: PathBuf,
    pub bitness: u8,
    pub reshade: bool,
    pub headers: bool,
    pub feeder: bool,
    pub lumenite: bool,
    pub dlss5_addon: bool,
    pub dlssnr: bool,
    pub dlss: bool,
    /// What the folder scan said, before any override.
    pub mode_detected: Mode,
    /// 32-bit only: `host64\dlss5-feed-host64.exe` and a 64-bit ReShade beside it.
    pub host_exe: bool,
    pub host_reshade: bool,
    /// Capcom RE Engine (needs REFramework before ReShade will run).
    pub re_engine: bool,
    pub reframework: bool,
    /// RenoDX game mod this tool installed, from its manifest.
    pub renodx_mod: Option<String>,
    /// Other RenoDX game mods found in the folder (not ours, not the DLSS 5 add-on).
    pub foreign_renodx: Vec<String>,
    /// Anti-cheat found (files or exe name), whether or not the refusal is overridden.
    pub anticheat: Option<&'static str>,
    pub problems: Vec<String>,
}

pub const IGNORE_ANTICHEAT_ENV: &str = "DLSS5ONECLICK_IGNORE_ANTICHEAT";
/// `feeder` or `native`: override the DLSS detection (a stray `nvngx_dlss.dll`
/// makes a game without DLSS look native; some games load DLSS from elsewhere).
pub const MODE_ENV: &str = "DLSS5ONECLICK_MODE";

pub fn mode_override() -> Option<Mode> {
    match std::env::var(MODE_ENV).ok()?.to_ascii_lowercase().as_str() {
        "feeder" | "nodlss" | "no-dlss" => Some(Mode::Feeder),
        "native" | "dlss" => Some(Mode::Native),
        _ => None,
    }
}

/// GUI dropdown / `--mode=`: same switch as the environment variable.
pub fn set_mode_override(m: Option<Mode>) {
    match m {
        Some(Mode::Feeder) => std::env::set_var(MODE_ENV, "feeder"),
        Some(Mode::Native) => std::env::set_var(MODE_ENV, "native"),
        None => std::env::remove_var(MODE_ENV),
    }
}

pub fn ignore_anticheat() -> bool {
    std::env::var_os(IGNORE_ANTICHEAT_ENV).is_some()
}

/// GUI checkbox / `--ignore-anticheat`: same switch as the environment variable.
pub fn set_ignore_anticheat(on: bool) {
    if on {
        std::env::set_var(IGNORE_ANTICHEAT_ENV, "1");
    } else {
        std::env::remove_var(IGNORE_ANTICHEAT_ENV);
    }
}

impl GameStatus {
    pub fn game_dir(&self) -> &Path {
        self.exe.parent().expect("exe has a parent")
    }
    pub fn is32(&self) -> bool {
        self.bitness == 32
    }
    /// Where the DLSS 5 add-on and the NVIDIA DLLs live: beside the exe for a
    /// 64-bit game, in `host64\` for a 32-bit one.
    pub fn consumer_dir(&self) -> PathBuf {
        if self.is32() {
            self.game_dir().join(HOST_DIR)
        } else {
            self.game_dir().to_path_buf()
        }
    }
    pub fn needs_bridge(&self) -> bool {
        self.mode == Mode::Native && self.api == Api::Dx11
    }
    pub fn complete(&self) -> bool {
        match self.mode {
            Mode::Feeder => {
                self.reshade
                    && self.headers
                    && self.feeder
                    && self.lumenite
                    && self.dlss5_addon
                    && self.dlssnr
                    && self.dlss
                    && (!self.is32() || (self.host_exe && self.host_reshade))
            }
            Mode::Native => {
                (self.opti && self.dlssnr)
                    || (self.reshade
                        && self.dlss5_addon
                        && self.dlssnr
                        && (!self.needs_bridge() || self.bridge))
            }
        }
    }
}

pub fn inspect(exe: &Path) -> Result<GameStatus> {
    if !exe.is_file() {
        bail!("game executable not found: {}", exe.display());
    }
    let d = exe.parent().context("exe has no parent directory")?;
    let bitness = exe_bitness(exe)?;
    let shaders = d.join("reshade-shaders").join("Shaders");
    let textures = d.join("reshade-shaders").join("Textures");
    let mut problems = Vec::new();
    let anticheat = detect_anticheat(d).or_else(|| known_anticheat_exe(exe));
    if let Some(ac) = anticheat {
        if !ignore_anticheat() {
            problems.push(format!(
                "{ac} anti-cheat found in this game. ReShade add-on injection is what it detects: kick at best, ban at worst. Refused."
            ));
        }
    }
    let gpu = gpu::best();
    let skip_gpu = std::env::var_os("DLSS5ONECLICK_SKIP_GPU_CHECK").is_some();
    if let Some((g, t)) = &gpu {
        if !t.can_run() && !skip_gpu {
            problems.push(format!(
                "GPU is {} ({}): the DLSS 5 model runs on NVIDIA RTX only (it needs tensor cores and NGX). Misdetected? Set DLSS5ONECLICK_SKIP_GPU_CHECK=1.",
                g.name,
                t.label()
            ));
        }
    }
    // A d3d9.dll is usually a wrapper this tool cannot work behind — except
    // dgVoodoo2, which is exactly how a D3D9 game reaches D3D11 and then the
    // Feeder (verified working on Dead or Alive 5 Last Round, #17).
    if d.join("d3d9.dll").is_file() && !d.join(RESHADE_PROXY).is_file() && !is_dgvoodoo(d) {
        problems.push(
            "A d3d9.dll proxy is present that is not dgVoodoo2. DirectX 9 itself is not a dead              end -- DLSS 5 needs a D3D11/12 device, and dgVoodoo2 provides one, which is how a              D3D9 game can work here (#17, #37) -- but this tool cannot install behind another              wrapper. Replace it with dgVoodoo 2.87.3 (MS\\x86\\D3D9.dll plus dgVoodoo.conf,              OutputAPI = bestavailable) and run Install again."
                .into(),
        );
    }
    let api = detect_api(exe);
    if api == Api::Vulkan {
        problems.push(
            "This is a Vulkan game. ReShade reaches Vulkan through a registered Vulkan \
             layer, not through the dxgi.dll this tool installs, so nothing here would \
             ever load -- no ReShade overlay and no log. Install ReShade with its own \
             setup and tick the Vulkan option, then use DLSS5-Feeder's Vulkan layer \
             (layer-x64 in its zip). This tool covers Direct3D 10, 11 and 12 only."
                .into(),
        );
    }
    let is32 = bitness == 32;
    if is32 && api == Api::Dx12 {
        problems.push(
            "32-bit game on Direct3D 12: DLSS5-Feeder's 32-bit add-on covers Direct3D 9 (through dgVoodoo2), 10 and 11, but not 12."
                .into(),
        );
    }
    // 32-bit: NGX is 64-bit only, so the game can never carry its own DLSS;
    // everything 64-bit goes to host64\ and the Feeder's addon32 sits in-game.
    let cdir = if is32 {
        d.join(HOST_DIR)
    } else {
        d.to_path_buf()
    };
    let feeder = if is32 {
        d.join(FEEDER_ADDON32).is_file() && shaders.join(FEEDER_FX).is_file()
    } else {
        d.join(FEEDER_ADDON).is_file() && shaders.join(FEEDER_FX).is_file()
    };
    let mode_detected = if !is32 && game_ships_dlss(d) {
        Mode::Native
    } else {
        Mode::Feeder
    };
    let mode = if is32 {
        Mode::Feeder
    } else {
        mode_override().unwrap_or(mode_detected)
    };
    let renodx_mod = fs::read_to_string(d.join(RENODX_MANIFEST))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|n| !n.is_empty() && d.join(n).is_file());
    Ok(GameStatus {
        mode,
        api,
        bridge: d.join(BRIDGE_ADDON).is_file() || d.join("dlss5-dx11-bridge.addon64").is_file(),
        opti: d.join(OPTI_MANIFEST).is_file(),
        gpu,
        exe: exe.to_path_buf(),
        bitness,
        reshade: !d.join(OPTI_MANIFEST).is_file() && is_reshade_dll(&d.join(RESHADE_PROXY)),
        headers: RESHADE_HEADERS.iter().all(|h| shaders.join(h).is_file()),
        feeder,
        lumenite: shaders.join(LUMENITE_KERNEL_FX).is_file()
            && textures.join(LUMENITE_BLUENOISE).is_file(),
        dlss5_addon: cdir.join(DLSS5_ADDON).is_file(),
        dlssnr: cdir.join(DLSSNR_DLL).is_file(),
        dlss: cdir.join(DLSS_DLL).is_file(),
        mode_detected,
        host_exe: is32 && cdir.join(HOST_EXE).is_file(),
        host_reshade: is32 && is_reshade_dll(&cdir.join(RESHADE_PROXY)),
        re_engine: d.join(RE_ENGINE_PAK).is_file(),
        reframework: d.join(REFRAMEWORK_DLL).is_file(),
        renodx_mod: renodx_mod.clone(),
        foreign_renodx: crate::renodx::foreign_mods(d, renodx_mod.as_deref()),
        anticheat,
        problems,
    })
}

/// Helper/launcher executables that are never the game.
const NOT_GAME: [&str; 15] = [
    "unitycrashhandler",
    "unrealcefsubprocess",
    "crashreportclient",
    "easyanticheat",
    "vcredist",
    "vc_redist",
    "dxwebsetup",
    "dxsetup",
    "oalinst",
    "ue4prereqsetup",
    "ueprereqsetup",
    "installer",
    "uninstall",
    "unins",
    "setup",
];

fn is_helper_name(stem_lower: &str) -> bool {
    NOT_GAME.iter().any(|n| stem_lower.contains(n))
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Candidate game executables in `dir`, best first.
///
/// Looks in the folder itself and in any `*/Binaries/Win64/` (Unreal layout,
/// where ReShade must sit next to the `-Shipping.exe`, not the root launcher).
/// Keeps 64-bit PEs only, drops known helpers, then ranks: Unreal shipping
/// exe > name matches the folder name > larger file.
pub fn find_game_exes(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut push_dir = |d: &Path| {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) && p.is_file() {
                    found.push(p);
                }
            }
        }
    };
    push_dir(dir);
    // One and two levels down (bin/x64, bin/x64_dx12, Game/Binaries/Win64 ...), skipping
    // engine/content trees that never hold the launch exe.
    let skip = |p: &Path| {
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            n.as_str(),
            "engine"
                | "content"
                | "saved"
                | "intermediate"
                | "reshade-shaders"
                | "_commonredist"
                | "commonredist"
                | "redist"
                | "redistributables"
        ) || n.ends_with("_data")
    };
    if let Ok(rd1) = fs::read_dir(dir) {
        for d1 in rd1
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !skip(p))
        {
            push_dir(&d1);
            if let Ok(rd2) = fs::read_dir(&d1) {
                for d2 in rd2
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && !skip(p))
                {
                    push_dir(&d2);
                    let win64 = d2.join("Win64");
                    if win64.is_dir() {
                        push_dir(&win64);
                    }
                }
            }
        }
    }
    // The Engine tree is skipped above because it is full of helper exes, but
    // Satisfactory keeps its shipping exe in exactly one place inside it (#29).
    let eng = dir.join("Engine").join("Binaries").join("Win64");
    if eng.is_dir() {
        push_dir(&eng);
    }
    let folder = norm(dir.file_name().and_then(|n| n.to_str()).unwrap_or(""));
    let mut scored: Vec<(i64, PathBuf)> = found
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            if is_helper_name(&stem) || exe_bitness(&p).ok()? != 64 {
                return None;
            }
            let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0) as i64;
            let mut score: i64 = 0;
            let n = norm(&stem);
            // An Unreal `-Shipping.exe` is always the real game; the root exe
            // named after the folder (Expedition33_Steam.exe, 700 KB) is a bootstrapper.
            if stem.ends_with("-shipping") {
                score += 2_000_000_000;
            }
            if !folder.is_empty()
                && (n == folder || n.starts_with(&folder) || folder.starts_with(&n))
            {
                score += 1_000_000_000;
            }
            score += size.min(400_000_000);
            Some((score, p))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored.into_iter().map(|(_, p)| p).collect()
}

/// Accepts either a game exe or a game folder; returns the exe to use plus
/// every candidate found (empty when the input was already an exe).
/// Did this tool install into this folder? True when any marker or manifest
/// it writes is present. Used to group those games together (#nn) and to
/// decide whether a component may be refreshed.
pub fn installed_by_tool(dir: &Path) -> bool {
    [
        OPTI_MANIFEST,
        RENODX_MANIFEST,
        REFRAMEWORK_MARKER,
        DLSS_MARKER,
        DLSSNR_MARKER,
        RESHADE_MARKER,
        FEEDER_MARKER,
    ]
    .iter()
    .any(|m| dir.join(m).is_file())
}

pub fn resolve_target(input: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    if input.is_file() {
        return Ok((input.to_path_buf(), Vec::new()));
    }
    if input.is_dir() {
        let c = find_game_exes(input);
        return match c.first() {
            Some(first) => Ok((first.clone(), c)),
            None => bail!("no 64-bit game executable found in {}", input.display()),
        };
    }
    bail!("not found: {}", input.display())
}

#[cfg(test)]
pub mod testutil {
    use super::*;

    pub fn make_pe(path: &Path, machine: u16) -> PathBuf {
        let mut head = vec![0u8; 0x40];
        head[..2].copy_from_slice(b"MZ");
        head[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let mut pe = b"PE\0\0".to_vec();
        pe.extend_from_slice(&machine.to_le_bytes());
        pe.extend_from_slice(&[0u8; 18]);
        head.extend_from_slice(&pe);
        fs::write(path, head).unwrap();
        path.to_path_buf()
    }

    pub fn make_reshade_dll(path: &Path) -> PathBuf {
        let mut b = b"MZ".to_vec();
        b.extend(std::iter::repeat_n(0u8, 1 << 20));
        b.extend_from_slice(b"ReShade");
        b.extend_from_slice(&[0u8; 16]);
        fs::write(path, b).unwrap();
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {

    /// Unreal keeps its Agility runtime in a D3D12 subfolder, Unity players put
    /// it directly beside the exe. Missing the second layout is what made a
    /// 0x887E0003 look unexplainable (dlss5-bridge#24).
    #[test]
    fn agility_redist_found_in_both_layouts() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert!(has_agility_redist(d).is_none());

        fs::write(d.join("D3D12Core.dll"), b"x").unwrap();
        assert_eq!(has_agility_redist(d), Some(d.join("D3D12Core.dll")));

        fs::create_dir(d.join("D3D12")).unwrap();
        fs::write(d.join("D3D12").join("D3D12Core.dll"), b"x").unwrap();
        // The subfolder wins when a game somehow carries both.
        assert_eq!(
            has_agility_redist(d),
            Some(d.join("D3D12").join("D3D12Core.dll"))
        );
    }
    use super::testutil::*;
    use super::*;

    #[test]
    fn bitness_x64_and_x86() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            exe_bitness(&make_pe(&t.path().join("a.exe"), PE_X64)).unwrap(),
            64
        );
        assert_eq!(
            exe_bitness(&make_pe(&t.path().join("b.exe"), PE_X86)).unwrap(),
            32
        );
    }

    #[test]
    fn bitness_rejects_non_pe() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x.exe");
        fs::write(&p, b"hello").unwrap();
        assert!(exe_bitness(&p).is_err());
    }

    #[test]
    fn reshade_dll_needs_marker_and_size() {
        let t = tempfile::tempdir().unwrap();
        let small = t.path().join("dxgi.dll");
        fs::write(&small, b"ReShade").unwrap();
        assert!(!is_reshade_dll(&small));
        assert!(is_reshade_dll(&make_reshade_dll(
            &t.path().join("real.dll")
        )));
    }

    #[test]
    fn inspect_empty_and_32bit() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let st = inspect(&make_pe(&t.path().join("game.exe"), PE_X64)).unwrap();
        assert_eq!(st.bitness, 64);
        assert!(
            !st.reshade
                && !st.headers
                && !st.feeder
                && !st.lumenite
                && !st.dlss5_addon
                && !st.dlssnr
                && !st.dlss
        );
        assert!(!st.complete());
        assert!(st.problems.is_empty());

        let st = inspect(&make_pe(&t.path().join("g32.exe"), PE_X86)).unwrap();
        assert!(st.is32() && st.mode == Mode::Feeder);
        assert!(!st.problems.iter().any(|p| p.contains("32-bit")));
    }

    #[test]
    fn find_game_exes_skips_helpers_and_prefers_folder_name() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Fell & Sell");
        fs::create_dir_all(&d).unwrap();
        make_pe(&d.join("UnityCrashHandler64.exe"), PE_X64);
        make_pe(&d.join("tool32.exe"), PE_X86);
        make_pe(&d.join("Fell & Sell.exe"), PE_X64);
        let c = find_game_exes(&d);
        assert_eq!(c, vec![d.join("Fell & Sell.exe")]);
        let (exe, all) = resolve_target(&d).unwrap();
        assert_eq!(exe, d.join("Fell & Sell.exe"));
        assert_eq!(all.len(), 1);
    }

    /// Satisfactory (Epic): the launcher names FactoryGameEGS.exe in the root,
    /// but the real game is the shipping exe under Engine\Binaries\Win64 (#29).
    /// A Direct3D 10 game imports d3d10_1.dll, and often d3d9.dll too for its
    /// D3DPERF debug markers - which is how DMC4:SE ends up looking like a
    /// DirectX 9 game to a naive scan.
    /// OptiScaler's dxgi.dll says "ReShade" because it can load ReShade; only
    /// a real ReShade build also carries crosire's name (#the update refusal).
    #[test]
    fn optiscaler_is_not_mistaken_for_reshade() {
        assert!(is_reshade_image(
            b"...crosire's ReShade version '6.8.0.2155'..."
        ));
        assert!(!is_reshade_image(
            b"OptiScaler ... LoadReshade ... ReShade64.dll ..."
        ));
        // A ReShade build without the author's name is still ReShade.
        assert!(is_reshade_image(b"ReShade effect runtime"));
        assert!(!is_reshade_image(b"some other dxgi wrapper"));
    }

    /// A Vulkan-only game imports vulkan-1.dll and no Direct3D; the dxgi.dll
    /// proxy can never load in one, so it has to be named rather than
    /// reported as "API unknown, assuming DX12" (#6).
    #[test]
    fn classify_imports_reads_vulkan() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(classify_imports(&s(&["vulkan-1.dll"])), Api::Vulkan);
        // A game offering both still takes the Direct3D path.
        assert_eq!(
            classify_imports(&s(&["vulkan-1.dll", "d3d12.dll"])),
            Api::Dx12
        );
        assert_eq!(
            classify_imports(&s(&["vulkan-1.dll", "d3d11.dll"])),
            Api::Dx11
        );
    }

    #[test]
    fn classify_imports_reads_d3d10() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            classify_imports(&s(&["d3d10_1.dll", "d3d9.dll"])),
            Api::Dx10
        );
        assert_eq!(classify_imports(&s(&["d3d10.dll"])), Api::Dx10);
        // The newer APIs still win when both are imported.
        assert_eq!(
            classify_imports(&s(&["d3d10_1.dll", "d3d11.dll"])),
            Api::Dx11
        );
        assert_eq!(
            classify_imports(&s(&["d3d10_1.dll", "d3d12.dll"])),
            Api::Dx12
        );
        assert_eq!(classify_imports(&s(&["kernel32.dll"])), Api::Unknown);
    }

    #[test]
    fn find_game_exes_finds_shipping_exe_under_engine_binaries() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("SatisfactoryEarlyAccess");
        let eng = d.join("Engine").join("Binaries").join("Win64");
        fs::create_dir_all(&eng).unwrap();
        make_pe(&d.join("FactoryGameEGS.exe"), PE_X64);
        let real = make_pe(&eng.join("FactoryGameEGS-Win64-Shipping.exe"), PE_X64);
        make_pe(&eng.join("CrashReportClient.exe"), PE_X64);
        let (exe, all) = resolve_target(&d).unwrap();
        assert_eq!(exe, real);
        assert!(all.contains(&d.join("FactoryGameEGS.exe")));
    }

    #[test]
    fn find_game_exes_skips_redistributables_and_setup_exes() {
        // Red Dead Redemption 2: RDR2.exe in the root, a much larger
        // Social-Club-Setup.exe under Redistributables\ (#22).
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Red Dead Redemption 2");
        fs::create_dir_all(d.join("Redistributables").join("SocialClub")).unwrap();
        let game = make_pe(&d.join("RDR2.exe"), PE_X64);
        let big = make_pe(
            &d.join("Redistributables")
                .join("SocialClub")
                .join("Social-Club-Setup.exe"),
            PE_X64,
        );
        fs::write(&big, [b"MZ".as_slice(), &[0u8; 4_000_000]].concat()).unwrap();
        make_pe(
            &d.join("Redistributables")
                .join("SocialClub")
                .join("Social-Club-Setup.exe"),
            PE_X64,
        );
        let found = find_game_exes(&d);
        assert_eq!(found.first(), Some(&game));
        assert!(!found
            .iter()
            .any(|p| p.to_string_lossy().contains("Redistributables")));
    }

    #[test]
    fn find_game_exes_unreal_layout_prefers_shipping() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("SomeGame");
        let bin = d.join("SomeGame").join("Binaries").join("Win64");
        fs::create_dir_all(&bin).unwrap();
        make_pe(&d.join("SomeGame.exe"), PE_X64);
        make_pe(&bin.join("SomeGame-Win64-Shipping.exe"), PE_X64);
        make_pe(&bin.join("CrashReportClient.exe"), PE_X64);
        let c = find_game_exes(&d);
        assert_eq!(c[0], bin.join("SomeGame-Win64-Shipping.exe"));
        assert_eq!(c.len(), 2);
        assert!(resolve_target(&t.path().join("nope")).is_err());
        let empty = t.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(resolve_target(&empty).is_err());
    }

    #[test]
    fn native_mode_when_game_ships_dlss() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        let st = inspect(&exe).unwrap();
        assert_eq!(st.mode, Mode::Feeder);
        assert_eq!(st.api, Api::Unknown);
        // nested nvngx_dlss.dll (Unreal-style plugin folder) -> native
        let deep = d.join("Engine").join("Plugins").join("DLSS");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join(DLSS_DLL), b"x").unwrap();
        let st = inspect(&exe).unwrap();
        assert_eq!(st.mode, Mode::Native);
        assert!(!st.complete());
        // our own copy (with sidecar marker) does not count
        fs::remove_file(deep.join(DLSS_DLL)).unwrap();
        fs::write(d.join(DLSS_DLL), b"x").unwrap();
        fs::write(d.join(DLSS_MARKER), b"").unwrap();
        assert_eq!(inspect(&exe).unwrap().mode, Mode::Feeder);
        // Streamline runtime alone is a DLSS signal
        fs::write(d.join("sl.interposer.dll"), b"x").unwrap();
        assert_eq!(inspect(&exe).unwrap().mode, Mode::Native);
    }

    #[test]
    fn pe_imports_handles_stub_pe() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), PE_X64);
        assert!(pe_imports(&exe).is_empty());
        assert_eq!(detect_api(&exe), Api::Unknown);
    }

    #[test]
    fn dgvoodoo_d3d9_is_allowed_other_d3d9_is_not() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X86);
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        fs::write(d.join("d3d9.dll"), b"MZ some other wrapper").unwrap();
        assert!(inspect(&exe)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("d3d9.dll proxy")));
        fs::write(d.join("dgVoodoo.conf"), b"[General]").unwrap();
        assert!(is_dgvoodoo(d));
        assert!(!inspect(&exe)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("d3d9.dll proxy")));
        fs::remove_file(d.join("dgVoodoo.conf")).unwrap();
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        assert!(is_dgvoodoo(d));
    }

    #[test]
    fn anticheat_detected_and_refused() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        assert_eq!(detect_anticheat(d), None);
        fs::create_dir_all(d.join("tools")).unwrap();
        fs::write(d.join("tools").join("GGSetup.exe"), b"x").unwrap();
        assert_eq!(detect_anticheat(d), Some("GameGuard"));
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = inspect(&exe).unwrap();
        assert!(st.problems.iter().any(|p| p.contains("GameGuard")));
        assert_eq!(st.anticheat, Some("GameGuard"));
        fs::remove_dir_all(d.join("tools")).unwrap();
        fs::create_dir_all(
            d.join("Game")
                .join("Binaries")
                .join("Win64")
                .join("EasyAntiCheat"),
        )
        .unwrap();
        assert_eq!(detect_anticheat(d), Some("Easy Anti-Cheat"));
        fs::remove_dir_all(d.join("Game")).unwrap();
        fs::write(d.join("Foo_BE.exe"), b"x").unwrap();
        assert_eq!(detect_anticheat(d), Some("BattlEye"));
    }

    #[test]
    fn inspect_complete() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        make_reshade_dll(&d.join("dxgi.dll"));
        let sh = d.join("reshade-shaders").join("Shaders");
        let tx = d.join("reshade-shaders").join("Textures");
        fs::create_dir_all(&sh).unwrap();
        fs::create_dir_all(&tx).unwrap();
        for f in [FEEDER_ADDON, DLSS5_ADDON, DLSSNR_DLL, DLSS_DLL] {
            fs::write(d.join(f), b"x").unwrap();
        }
        for h in RESHADE_HEADERS {
            fs::write(sh.join(h), "// header").unwrap();
        }
        fs::write(sh.join(FEEDER_FX), "technique DLSS5_Feed {}").unwrap();
        fs::write(sh.join(LUMENITE_KERNEL_FX), "technique Lumenite_Kernel {}").unwrap();
        fs::write(tx.join(LUMENITE_BLUENOISE), b"png").unwrap();
        assert!(inspect(&exe).unwrap().complete());
    }
}
