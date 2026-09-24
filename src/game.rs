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
/// mavismmg/MFGAdaUnlock-RenoDx: a ReShade add-on that lifts NVIDIA's RTX 50
/// gate on multi-frame generation and corrects the temporal midpoint, entirely
/// in mapped memory. One file, MIT, nothing in the game folder modified.
pub const MFG_ADDON: &str = "renodx-mfgunlock.addon64";
/// NVIDIA's frame-generation runtime. The MFG add-on validates this provider by
/// build and refuses anything it does not know, so the version beside the game
/// decides whether the unlock does anything at all.
pub const DLSSG_DLL: &str = "nvngx_dlssg.dll";
pub const DLSSG_MARKER: &str = "nvngx_dlssg.dll.dlss5oneclick";
/// The game's own provider, moved aside before ours goes in. Never deleted:
/// Remove puts it back.
pub const DLSSG_BACKUP: &str = "nvngx_dlssg.dll.original";
/// Sidecar for a dgVoodoo `d3d9.dll` this tool downloaded. Remove used to leave
/// dgVoodoo behind on every DX9 game, so a game that would not start kept not
/// starting after Remove and there was nothing saying which file to delete (#91).
/// A second line, `conf-ours`, marks a `dgVoodoo.conf` this tool created rather
/// than merged into someone's existing one.
pub const DGVOODOO_MARKER: &str = "d3d9.dll.dlss5oneclick";
pub const DGVOODOO_CONF: &str = "dgVoodoo.conf";
/// matiasLombo's neural-upstream add-on. The name is not ours to choose: the
/// NGX snippet gates feature creation on the calling module's path containing
/// `nvngx.dll`, and under any other name it returns 0xBAD00002 and does nothing.
pub const UPSTREAM_ADDON: &str = "nvngx.dll.addon64";
/// kibblerz's DLSS5 ReShade AIO: its own NR + super resolution + frame
/// generation pipeline as one ReShade add-on, for games with no DLSS of their
/// own. Takes the place of the Feeder and the RenoDX add-on together.
pub const AIO_ADDON: &str = "standalone-dlssnr.addon64";
/// Files this tool wrote for an AIO install, one path per line, tag in the header.
pub const AIO_MANIFEST: &str = ".dlss5oneclick-aio-manifest";
/// Files this tool wrote for an OptiScaler install, one path per line.
pub const OPTI_MANIFEST: &str = ".dlss5oneclick-optiscaler-manifest";
/// Sidecar written next to an `nvngx_dlss.dll` this tool placed, so it is never mistaken for the game's own.
pub const DLSS_MARKER: &str = "nvngx_dlss.dll.dlss5oneclick";
/// Same idea for the neural model and for ReShade: the release tag / version this
/// tool placed, so Install can tell "ours and stale" from "the user's own".
pub const DLSSNR_MARKER: &str = "nvngx_dlssnr.dll.dlss5oneclick";
pub const RESHADE_MARKER: &str = "dxgi.dll.dlss5oneclick";
/// The DLSS 5 add-on release tag this tool placed. Every build carries the same
/// embedded FileVersion (0.2026.0828.0517 in 4.55 and 4.70 alike), so the
/// Feeder's host names them all "v4.6 engine" and warns about a combination the
/// classic build is not part of. The tag is the only way to tell them apart.
pub const DLSS5_ADDON_MARKER: &str = "renodx-dlss5.addon64.dlss5oneclick";
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

/// True when `dinput8.dll` is really REFramework rather than something else
/// wearing that name.
///
/// `dinput8.dll` is a proxy slot, not a product: REFramework uses it, and so do
/// the RTX 20/30 frame-generation mods (`dlssg_for_sm75` and `dlssg_for_sm86`
/// ship `dinput8.dll` and `version.dll` to intercept nvapi and spoof the GPU
/// architecture). Testing only for the file's presence meant an RE Engine game
/// with one of those installed looked like it already had REFramework, so the
/// install skipped it — and RE Engine games crash under ReShade without it.
pub fn is_reframework_dll(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    // praydog's own name and the project's are both in every build; the
    // frame-generation proxies carry neither.
    has(b"REFramework") || has(b"praydog")
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
    /// Direct3D 9. DLSS 5 needs a D3D11/12 device; the supported path is
    /// dgVoodoo2 translating D3D9 to D3D11 so this tool's `dxgi.dll` ReShade
    /// can load (verified on Dead or Alive 5 Last Round, #17, #37). Install
    /// downloads official dgVoodoo 2.87.5 into the game folder when missing.
    /// Aion loads system d3d9.dll by name (#16) so local ReShade d3d9.dll hooks it.
    Dx9,
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
    /// No static D3D/Vulkan import (loaded at runtime). Treated like DX12 for
    /// the install plan; DX9 is classified separately when `d3d9.dll` is imported.
    Unknown,
}

impl Api {
    pub fn label(self) -> &'static str {
        match self {
            Api::Dx9 => "DX9",
            Api::Dx10 => "DX10",
            Api::Dx11 => "DX11",
            Api::Dx12 => "DX12",
            Api::Vulkan => "Vulkan",
            Api::Unknown => "API unknown, assuming DX12",
        }
    }
}

/// The head of a PE file through the end of the section that holds its
/// import directory — everything `pe_imports` and `pe_import_fns` index,
/// as a prefix so their file offsets stay valid. Crimson Desert's exe is
/// 358 MB and reading all of it, then twelve engine DLLs the same way,
/// froze the window for a minute on clicking the game; the import section
/// ends a few tens of MB in.
fn pe_import_prefix(exe: &Path) -> Option<Vec<u8>> {
    let mut f = fs::File::open(exe).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    let mut head = vec![0u8; len.min(64 * 1024)];
    f.read_exact(&mut head).ok()?;
    let rd32 = |o: usize| -> Option<u32> {
        head.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let rd16 =
        |o: usize| -> Option<u16> { head.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };
    let end = (|| -> Option<usize> {
        if head.get(..2)? != b"MZ" {
            return None;
        }
        let pe = rd32(0x3C)? as usize;
        if head.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let nsec = rd16(coff + 2)? as usize;
        let opt_size = rd16(coff + 16)? as usize;
        let opt = coff + 20;
        let dd_off = match rd16(opt)? {
            0x20B => 112,
            0x10B => 96,
            _ => return None,
        };
        let import_rva = rd32(opt + dd_off + 8)? as usize;
        if import_rva == 0 {
            return Some(head.len());
        }
        let sec = opt + opt_size;
        (0..nsec).find_map(|i| {
            let s = sec + i * 40;
            let (va, size, raw, raw_size) = (
                rd32(s + 12)? as usize,
                rd32(s + 16)? as usize,
                rd32(s + 20)? as usize,
                rd32(s + 8)? as usize,
            );
            (import_rva >= va && import_rva < va + size).then_some(raw + raw_size.max(size))
        })
    })()?;
    let end = end.min(len);
    if end <= head.len() {
        head.truncate(end);
        return Some(head);
    }
    let mut data = head;
    data.resize(end, 0);
    f.seek(SeekFrom::Start(64 * 1024)).ok()?;
    f.read_exact(&mut data[64 * 1024..]).ok()?;
    Some(data)
}

/// Lower-cased DLL names from the exe's static import table. Empty on any parse problem.
/// Every function name the PE imports from `dll` (lowercased).
///
/// Needed because importing `d3d9.dll` does not make a game a Direct3D 9 game:
/// engines from the D3D9 era onward import it for the `D3DPERF_*` debug markers
/// alone and render with something far newer, which is how RDR2 read as
/// DirectX 9 (#53). A real D3D9 renderer calls `Direct3DCreate9`.
pub fn pe_import_fns(exe: &Path, dll: &str) -> Vec<String> {
    let Some(data) = pe_import_prefix(exe) else {
        return vec![];
    };
    let rd32 = |o: usize| -> Option<u32> {
        data.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let rd64 = |o: usize| -> Option<u64> {
        data.get(o..o + 8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    };
    let rd16 =
        |o: usize| -> Option<u16> { data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };
    let cstr = |off: usize| -> String {
        let end = data[off..]
            .iter()
            .position(|&b| b == 0)
            .map(|n| off + n)
            .unwrap_or(off);
        String::from_utf8_lossy(&data[off..end]).to_ascii_lowercase()
    };
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
        let (dd_off, pe32_plus) = match magic {
            0x20B => (112, true),
            0x10B => (96, false),
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
        let want = dll.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut desc = to_off(import_rva)?;
        for _ in 0..512 {
            let name_rva = rd32(desc + 12)? as usize;
            if name_rva == 0 && rd32(desc)? == 0 {
                break;
            }
            let is_ours = to_off(name_rva).map(&cstr).is_some_and(|n| n == want);
            if is_ours {
                // OriginalFirstThunk when present, else FirstThunk.
                let thunk_rva = match rd32(desc)? {
                    0 => rd32(desc + 16)? as usize,
                    v => v as usize,
                };
                if let Some(mut t) = to_off(thunk_rva) {
                    for _ in 0..4096 {
                        let (entry, step) = if pe32_plus {
                            (rd64(t)?, 8)
                        } else {
                            (rd32(t)? as u64, 4)
                        };
                        if entry == 0 {
                            break;
                        }
                        let by_ordinal = if pe32_plus {
                            entry & (1 << 63) != 0
                        } else {
                            entry & (1 << 31) != 0
                        };
                        if !by_ordinal {
                            // IMAGE_IMPORT_BY_NAME: 2-byte hint, then the name.
                            if let Some(off) = to_off(entry as usize & 0x7fff_ffff) {
                                out.push(cstr(off + 2));
                            }
                        }
                        t += step;
                    }
                }
            }
            desc += 20;
        }
        Some(out)
    };
    parse().unwrap_or_default()
}

pub fn pe_imports(exe: &Path) -> Vec<String> {
    let Some(data) = pe_import_prefix(exe) else {
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
/// 10 game actually imports (often alongside `d3d9.dll`); higher APIs win so
/// those are not misclassified as DirectX 9.
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
    } else if has("d3d9.dll") {
        Api::Dx9
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

/// An import that only a renderer newer than Direct3D 9 would carry.
///
/// Direct3D 9 predates DXGI and never uses it, so `dxgi.dll` settles it alone.
/// Beyond that, upscaler and latency libraries are shipped per graphics API and
/// name it in the file: FidelityFX FSR2's backends are `..._dx12_x64.dll` and
/// `..._vk_x64.dll` (AMD ships one per target API), and NVIDIA's Reflex library
/// for Vulkan is `nvlowlatencyvk.dll`. Red Dead Redemption 2 imports all three
/// while creating its real device at runtime, and statically imports
/// `Direct3DCreate9Ex` besides, so neither the marker rule nor DXGI alone
/// caught it (#53).
fn implies_newer_than_d3d9(import: &str) -> bool {
    let stem = import.strip_suffix(".dll").unwrap_or(import);
    matches!(
        stem,
        "dxgi" | "d3d10" | "d3d10_1" | "d3d11" | "d3d12" | "vulkan-1"
    ) || stem.contains("dx11")
        || stem.contains("dx12")
        || stem.contains("vulkan")
        || stem.ends_with("vk")
        || stem.contains("_vk_")
}

/// True when the PE imports `d3d9.dll` for something other than the
/// `D3DPERF_*` debug markers *and* carries nothing that implies a newer API.
fn d3d9_is_the_renderer(pe: &Path) -> bool {
    let imports = pe_imports(pe);
    if imports.iter().any(|i| implies_newer_than_d3d9(i)) {
        return false;
    }
    let fns = pe_import_fns(pe, "d3d9.dll");
    fns.is_empty() || fns.iter().any(|f| !f.starts_with("d3dperf_"))
}

pub fn detect_api(exe: &Path) -> Api {
    use classify_imports as classify;
    let mut api = classify(&pe_imports(exe));
    if api == Api::Dx9 && !d3d9_is_the_renderer(exe) {
        api = Api::Unknown;
    }
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
    let mut seen_dx9 = false;
    for (_, dll) in dlls.into_iter().take(12) {
        match classify(&pe_imports(&dll)) {
            Api::Dx12 => return Api::Dx12,
            Api::Dx11 => seen_dx11 = true,
            // Aion renders through XRenderD3D9.dll rather than from the exe,
            // so the renderer DLL is the only place the API shows (#16). The
            // marker-only case is excluded here too (#53).
            Api::Dx9 => seen_dx9 |= d3d9_is_the_renderer(&dll),
            Api::Dx10 | Api::Vulkan | Api::Unknown => {}
        }
    }
    if seen_dx9 && !seen_dx11 {
        return Api::Dx9;
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
        Ok(b) => dll_mentions_dgvoodoo(&b),
        Err(_) => false,
    }
}

/// Official dgVoodoo builds embed the product name as UTF-16LE in the PE
/// resources; older / debug builds may use ASCII. Either counts.
fn dll_mentions_dgvoodoo(b: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"dgVoodoo";
    if b.windows(NEEDLE.len())
        .any(|w| w.eq_ignore_ascii_case(NEEDLE))
    {
        return true;
    }
    let n = NEEDLE.len() * 2;
    if b.len() < n {
        return false;
    }
    b.windows(n).any(|w| {
        NEEDLE
            .iter()
            .enumerate()
            .all(|(i, &c)| w[i * 2].eq_ignore_ascii_case(&c) && w[i * 2 + 1] == 0)
    })
}

/// Anti-cheat present in the install tree, by the files those systems ship.
/// ReShade add-on injection is exactly what they look for: kicks at best, bans
/// at worst. Verified file names: EAC `EasyAntiCheat[_EOS]/EasyAntiCheat_EOS_Setup.exe`,
/// BattlEye `BattlEye/BEService_x64.exe`, `Install_BattlEye.bat`, `*_BE.exe`,
/// GameGuard `tools/GGSetup.exe` or a `GameGuard` folder, EA Javelin an
/// `EAAntiCheat` folder or `EAAntiCheat.GameServiceLauncher.exe/.dll` beside
/// the exe, Tencent ACE an `AntiCheatExpert` folder.
/// A folder with thousands of entries is an asset store (extracted game
/// archives, a texture dump), never where a DLL, an anti-cheat, or an exe
/// lives. Crimson Desert's bin64 carried 318,000 files in two such folders
/// and every walk here crossed them; clicking the game froze the window for
/// a minute. Counting stops at the cap, so the check itself stays cheap.
pub fn huge_dir(d: &Path) -> bool {
    const CAP: usize = 2000;
    fs::read_dir(d)
        .map(|rd| rd.take(CAP + 1).count() > CAP)
        .unwrap_or(false)
}

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
                // EA Javelin is kernel-mode and, by EA's own description, also
                // guards single-player against tampering — so it is in play even
                // in a game nobody is competing in (#21).
                if n == "eaanticheat" {
                    return Some("EA Javelin Anticheat");
                }
                // Tencent's Anti-Cheat Expert, kernel-mode. Arknights: Endfield
                // ships it as an AntiCheatExpert folder beside the exe; with
                // OptiScaler in place the game crashed at start (#113).
                if n == "anticheatexpert" {
                    return Some("Anti-Cheat Expert (ACE)");
                }
                if depth > 0 && !huge_dir(&p) {
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
                if n.starts_with("eaanticheat") {
                    return Some("EA Javelin Anticheat");
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
        // Retail WoW has blocked a dxgi.dll override outright (11.1.7.61965,
        // and again in 11.2.0: "Your 3D accelerator card is not supported"),
        // and Blizzard's position is that injecting into the render chain is
        // against its terms whether or not it is blocked that week. A 3.3.5a
        // private-server client carries the same exe name and its own rules,
        // so the warning is the same and the override tick is right there (#45).
        "wow.exe" | "wowclassic.exe" => Some("Blizzard Warden (World of Warcraft)"),
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
                // A frame-generation runtime this tool placed for the AIO is
                // not the game's either.
                if (n == DLSSG_DLL && !p.with_file_name(DLSSG_MARKER).is_file())
                    || n == "nvngx_dlssd.dll"
                    || (n.starts_with("sl.") && n.ends_with(".dll"))
                {
                    return true;
                }
            } else if depth > 0 && p.is_dir() && !huge_dir(&p) && walk(&p, depth - 1) {
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

/// Unreal-style layout: `…/Binaries/Win64` or a `-Shipping.exe` next to UE folders.
pub fn unreal_likely(exe: &Path, game_dir: &Path) -> bool {
    let stem = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if stem.contains("-shipping") || stem.ends_with("shipping") {
        return true;
    }
    if game_dir
        .parent()
        .and_then(|b| b.file_name())
        .is_some_and(|n| n.eq_ignore_ascii_case("binaries"))
    {
        return true;
    }
    let proj = game_dir
        .parent()
        .filter(|b| {
            b.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("binaries"))
        })
        .and_then(|b| b.parent());
    if let Some(proj) = proj {
        if proj.join("Plugins").is_dir() || proj.join("Content").is_dir() {
            return true;
        }
    }
    false
}

/// UnityPlayer.dll beside the exe (or one folder up for some layouts).
pub fn unity_likely(game_dir: &Path) -> bool {
    game_dir.join("UnityPlayer.dll").is_file()
        || game_dir
            .parent()
            .is_some_and(|p| p.join("UnityPlayer.dll").is_file())
}

/// Lightweight install-time RT / ray-reconstruction hints (not a DXR hook).
/// Looks for known ini keys, folder names, and `nvngx_dlssd.dll`.
pub fn rt_likely(game_dir: &Path) -> bool {
    fn name_hit(n: &str) -> bool {
        let n = n.to_ascii_lowercase();
        n.contains("raytracing")
            || n.contains("ray_tracing")
            || n.contains("rtxgi")
            || n.contains("dlssd")
            || n == "nvngx_dlssd.dll"
            || n.contains("lumen") && n.contains("hardware")
    }
    fn walk_names(d: &Path, depth: u8) -> bool {
        let Ok(rd) = fs::read_dir(d) else {
            return false;
        };
        for e in rd.flatten() {
            let p = e.path();
            let n = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if name_hit(&n) {
                return true;
            }
            if p.is_file()
                && (n.ends_with(".ini") || n.ends_with(".cfg") || n.ends_with(".txt"))
                && file_mentions_rt(&p)
            {
                return true;
            }
            if depth > 0
                && p.is_dir()
                && !n.starts_with('.')
                && !huge_dir(&p)
                && walk_names(&p, depth - 1)
            {
                return true;
            }
        }
        false
    }
    fn file_mentions_rt(p: &Path) -> bool {
        let Ok(s) = fs::read_to_string(p) else {
            return false;
        };
        // Cap read cost: only scan first ~64 KiB worth of UTF-8 lossy via take on chars.
        let head: String = s.chars().take(64 * 1024).collect();
        let l = head.to_ascii_lowercase();
        l.contains("hardwareraytracing")
            || l.contains("r.raytracing")
            || l.contains("raytracing=")
            || l.contains("ray tracing")
            || l.contains("dlss-rr")
            || l.contains("rayreconstruction")
    }
    if walk_names(game_dir, 3) {
        return true;
    }
    // Unreal Plugins tree
    if let Some(proj) = game_dir
        .parent()
        .filter(|b| {
            b.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("binaries"))
        })
        .and_then(Path::parent)
    {
        if walk_names(&proj.join("Plugins"), 5) {
            return true;
        }
    }
    false
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
    /// What the import table said, before any `--api` override.
    pub api_detected: Api,
    /// 32-bit only: `host64\dlss5-feed-host64.exe` and a 64-bit ReShade beside it.
    pub host_exe: bool,
    pub host_reshade: bool,
    /// Capcom RE Engine (needs REFramework before ReShade will run).
    pub re_engine: bool,
    pub reframework: bool,
    /// matiasLombo's neural-upstream add-on is in the folder.
    pub upstream: bool,
    /// kibblerz's standalone AIO add-on is in the folder.
    pub aio: bool,
    /// The RTX 40 multi-frame-generation add-on is already beside the game.
    pub mfg: bool,
    /// Unreal-style layout / Shipping exe (heuristic).
    pub unreal_likely: bool,
    /// UnityPlayer.dll present (heuristic).
    pub unity_likely: bool,
    /// RT / DLSS-D / raytracing markers on disk (heuristic — not live DXR state).
    pub rt_likely: bool,
    /// RenoDX game mod this tool installed, from its manifest.
    pub renodx_mod: Option<String>,
    /// Other RenoDX game mods found in the folder (not ours, not the DLSS 5 add-on).
    pub foreign_renodx: Vec<String>,
    /// Anti-cheat found (files or exe name), whether or not the refusal is overridden.
    pub anticheat: Option<&'static str>,
    pub problems: Vec<String>,
}

/// A blank status for tests, so a test can say only the fields it cares about.
#[cfg(test)]
pub(crate) fn stub_status(mode: Mode, api: Api) -> GameStatus {
    GameStatus {
        mode,
        api,
        bridge: false,
        opti: false,
        gpu: Some((
            gpu::Gpu {
                name: "NVIDIA GeForce RTX 4060".into(),
                vendor: "NVIDIA".into(),
            },
            gpu::Tier::Rtx40,
        )),
        exe: PathBuf::from(r"C:\g\game.exe"),
        bitness: 64,
        reshade: false,
        headers: false,
        feeder: false,
        lumenite: false,
        dlss5_addon: false,
        dlssnr: false,
        dlss: false,
        mode_detected: mode,
        api_detected: api,
        host_exe: false,
        host_reshade: false,
        re_engine: false,
        reframework: false,
        upstream: false,
        aio: false,
        mfg: false,
        unreal_likely: false,
        unity_likely: false,
        rt_likely: false,
        renodx_mod: None,
        foreign_renodx: vec![],
        anticheat: None,
        problems: vec![],
    }
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

/// `dx11` or `dx12`: override the graphics-API detection. Detection reads
/// the import table and the engine DLLs beside the exe, and says "unknown,
/// assuming DX12" when neither speaks; a wrong answer picks the wrong route
/// (the DX11 bridge, the D3D12-only frame-generation gate).
pub const API_ENV: &str = "DLSS5ONECLICK_API";

pub fn api_override() -> Option<Api> {
    match std::env::var(API_ENV).ok()?.to_ascii_lowercase().as_str() {
        "dx11" | "d3d11" => Some(Api::Dx11),
        "dx12" | "d3d12" => Some(Api::Dx12),
        _ => None,
    }
}

/// GUI dropdown / `--api=`: same switch as the environment variable.
pub fn set_api_override(a: Option<Api>) {
    match a {
        Some(Api::Dx11) => std::env::set_var(API_ENV, "dx11"),
        Some(Api::Dx12) => std::env::set_var(API_ENV, "dx12"),
        _ => std::env::remove_var(API_ENV),
    }
}

pub fn ignore_anticheat() -> bool {
    std::env::var_os(IGNORE_ANTICHEAT_ENV).is_some()
}

/// Override for the GPU check. Named here rather than spelled out at each use
/// so the GUI tick and the environment variable cannot drift apart (#3).
pub const SKIP_GPU_CHECK_ENV: &str = "DLSS5ONECLICK_SKIP_GPU_CHECK";

pub fn skip_gpu_check() -> bool {
    std::env::var_os(SKIP_GPU_CHECK_ENV).is_some()
}

/// GUI checkbox: same switch as the environment variable. A GUI user cannot
/// set an env var for an already-running window, which left the refusal
/// naming a fix they could not reach (#3).
pub fn set_skip_gpu_check(on: bool) {
    if on {
        std::env::set_var(SKIP_GPU_CHECK_ENV, "1");
    } else {
        std::env::remove_var(SKIP_GPU_CHECK_ENV);
    }
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
    /// Why the ReShade engine cannot reach this game, when that is the case.
    /// A Vulkan game has no `dxgi.dll` to hook, so ReShade must be installed as
    /// a Vulkan layer by its own setup -- but OptiScaler reaches Vulkan games
    /// perfectly well, so this refuses one engine, not the game (#46,
    /// Indiana Jones and the Great Circle, which worked until 0.11.8).
    pub fn reshade_engine_problem(&self) -> Option<String> {
        (self.api == Api::Vulkan).then(|| {
            "This is a Vulkan game, so the ReShade engine cannot reach it: ReShade hooks \
             Vulkan through a registered layer, not through the dxgi.dll installed beside \
             the exe, and nothing here would ever load. Use the OptiScaler engine, which \
             does cover Vulkan. To use ReShade anyway, run its own setup, point it at this \
             exe and choose Vulkan, then follow DLSS5-Feeder's Vulkan instructions."
                .to_owned()
        })
    }

    pub fn is32(&self) -> bool {
        self.bitness == 32
    }
    /// DX9 without dgVoodoo2 yet: Install will download it into the game folder.
    pub fn needs_dgvoodoo(&self) -> bool {
        self.api == Api::Dx9 && !is_dgvoodoo(self.game_dir())
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
        // The AIO is its own consumer on either kind of game: ReShade, the
        // add-on, the model, and NVIDIA's DLSS runtime beside it.
        if self.aio && !self.opti {
            return self.reshade && self.dlssnr && self.dlss;
        }
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
                // Either neural consumer counts: the RenoDX add-on, or the
                // experimental Neural Upstream one that stands in its place.
                (self.opti && self.dlssnr)
                    || (self.reshade
                        && (self.dlss5_addon || self.upstream)
                        && self.dlssnr
                        && (!self.needs_bridge() || self.bridge))
            }
        }
    }
}

/// The `Content` folder of a Game Pass / Microsoft Store install: the one with
/// `MicrosoftGame.config`, at the exe's folder or a few levels up.
pub fn game_pass_content_dir(d: &Path) -> Option<PathBuf> {
    d.ancestors()
        .take(4)
        .find(|a| a.join("MicrosoftGame.config").is_file())
        .map(Path::to_path_buf)
}

/// Whether a file can be created in `d`: the one test that separates a
/// protected Store install from one that takes mods.
fn dir_writable(d: &Path) -> bool {
    let probe = d.join(".dlss5oneclick-write-probe");
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

/// Whether a Game Pass install ships NVIDIA's DLSS runtime anywhere under its
/// `Content` folder. The exe itself may be unreadable, but the DLLs are not.
fn game_pass_ships_dlss(content: &Path) -> bool {
    fn walk(d: &Path, depth: u8) -> bool {
        let Ok(rd) = fs::read_dir(d) else {
            return false;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                if p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(DLSS_DLL))
                {
                    return true;
                }
            } else if depth > 0 && p.is_dir() && !huge_dir(&p) && walk(&p, depth - 1) {
                return true;
            }
        }
        false
    }
    walk(content, 4)
}

pub fn inspect(exe: &Path) -> Result<GameStatus> {
    if !exe.is_file() {
        bail!("game executable not found: {}", exe.display());
    }
    let d = exe.parent().context("exe has no parent directory")?;
    // Game Pass installs are protected: the real exe is often unreadable, the
    // folder refuses writes, and a dxgi.dll beside a packaged exe is never
    // loaded. Every one of them read as "no DLSS, Feeder path" here, because
    // what the scan finds is gamelaunchhelper.exe, a stub with nothing in it.
    // A Game Pass / Store install is protected only sometimes. 0.13.16 refused
    // every folder with a MicrosoftGame.config, which took out installs that
    // had worked for their owners (Batman: Arkham Knight, Doom, Forza — #107;
    // Watch Dogs via Game Pass in #62) and the Steam copy of Forza Horizon 6,
    // which ships that file too (#106). What decides it is whether the folder
    // takes a write and the exe can be read, so that is what is checked.
    if let Some(content) = game_pass_content_dir(d) {
        let locked = fs::File::open(exe).is_err() || !dir_writable(d);
        if locked {
            let native = if game_pass_ships_dlss(&content) {
                "The game itself ships DLSS (nvngx_dlss.dll is in its folder), so it supports DLSS natively; only this install cannot be modified."
            } else {
                "No nvngx_dlss.dll anywhere in its folder, so this game has no DLSS of its own either."
            };
            bail!(
                "Game Pass / Microsoft Store copy ({}): Windows protects this install — the \
                 executable is locked or the folder refuses writes — so DLSS 5 cannot be \
                 installed on this copy. Some Store games are installed unprotected and work; \
                 this one is not. The same game from Steam, Epic or GOG works. {native}",
                content.display()
            );
        }
    }
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
    let skip_gpu = skip_gpu_check();
    if let Some((g, t)) = &gpu {
        if !t.can_run() && !skip_gpu {
            problems.push(format!(
                "GPU is {} ({}): the DLSS 5 model runs on NVIDIA RTX only (it needs tensor cores and NGX). \
                 Misdetected, or running through Remote Desktop? Tick the box below, or set {}=1.",
                g.name,
                t.label(),
                SKIP_GPU_CHECK_ENV
            ));
        }
    }
    // A d3d9.dll is usually a wrapper this tool cannot work behind — except
    // dgVoodoo2, which is exactly how a D3D9 game reaches D3D11 and then the
    // Feeder (verified working on Dead or Alive 5 Last Round, #17).
    if d.join("d3d9.dll").is_file() && !d.join(RESHADE_PROXY).is_file() && !is_dgvoodoo(d) {
        problems.push(
            "A d3d9.dll proxy is present that is not dgVoodoo2. DirectX 9 itself is not a dead              end -- DLSS 5 needs a D3D11/12 device, and dgVoodoo2 provides one, which is how a              D3D9 game can work here (#17, #37) -- but this tool cannot install behind another              wrapper. Replace it with dgVoodoo 2.87.5 (MS\\x86 or MS\\x64\\D3D9.dll plus              dgVoodoo.conf, OutputAPI = d3d11_fl11_0, VRAM >= 4096) and run Install again."
                .into(),
        );
    }
    let api_detected = detect_api(exe);
    let api = api_override().unwrap_or(api_detected);
    // Plain D3D9 (Gothic 3, Aion, etc.): ReShade is dxgi.dll here, which a
    // D3D9 process never loads. Install adds official dgVoodoo 2.87.5 so DX9
    // is not a hard refuse. A foreign non-dgVoodoo d3d9.dll still blocks above.
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
        upstream: d.join(UPSTREAM_ADDON).is_file(),
        aio: d.join(AIO_ADDON).is_file(),
        mfg: d.join(MFG_ADDON).is_file(),
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
        api_detected,
        host_exe: is32 && cdir.join(HOST_EXE).is_file(),
        host_reshade: is32 && is_reshade_dll(&cdir.join(RESHADE_PROXY)),
        re_engine: d.join(RE_ENGINE_PAK).is_file(),
        reframework: is_reframework_dll(&d.join(REFRAMEWORK_DLL)),
        unreal_likely: unreal_likely(exe, d),
        unity_likely: unity_likely(d),
        rt_likely: rt_likely(d),
        renodx_mod: renodx_mod.clone(),
        foreign_renodx: crate::renodx::foreign_mods(d, renodx_mod.as_deref()),
        anticheat,
        problems,
    })
}

/// Helper/launcher executables that are never the game.
const NOT_GAME: [&str; 17] = [
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
    // Ubisoft's Support\GDF\FirewallInstall.exe outranked ACBSP.exe (#105).
    "install",
    // Rockstar's 64-bit PlayGTAIV.exe launcher outranked the 32-bit game (#94).
    "playgtaiv",
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
/// Prefers 64-bit PEs, drops known helpers, then ranks: Unreal shipping
/// exe > name matches the folder name > larger file. A folder with no 64-bit
/// game at all falls back to its 32-bit exes, which the feeder drives through
/// its host64 helper (Max Payne, #17).
pub fn find_game_exes(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut push_dir = |d: &Path| {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                // Not every game launcher is named .exe: Aion ships aion.bin,
                // which is an ordinary PE (#16). Anything that is not really a
                // PE is dropped by the bitness read further down, so widening
                // the extension costs nothing.
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("exe") || x.eq_ignore_ascii_case("bin"))
                    && p.is_file()
                    && name != HOST_EXE.to_ascii_lowercase()
                {
                    found.push(p);
                }
            }
        }
    };
    push_dir(dir);
    // Subfolders that never hold a launch exe, skipped at every level.
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
                | "support"
                | "gdf"
                | "saved"
                | "intermediate"
                | "reshade-shaders"
                | "_commonredist"
                | "commonredist"
                | "redist"
                | "redistributables"
                // Our own 32-bit helper lives here. It is a 64-bit PE, so the
                // "prefer 64-bit" rule ranked it above the real 32-bit game and
                // the next Install treated the game as 64-bit — writing a
                // 64-bit ReShade into a 32-bit game's folder, which the game
                // cannot load, so the Home key did nothing (#69).
                | HOST_DIR
        ) || n.ends_with("_data")
    };
    // Down to four levels, which is where games actually put the launch exe:
    // bin/x64, Game/Binaries/Win64, and ph_ft/work/bin/x64 (Dying Light: The
    // Beast, #43). Engine and content trees are skipped at every level.
    fn walk(d: &Path, depth: u8, skip: &dyn Fn(&Path) -> bool, out: &mut dyn FnMut(&Path)) {
        if depth == 0 {
            return;
        }
        let Ok(rd) = fs::read_dir(d) else {
            return;
        };
        for sub in rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !skip(p) && !huge_dir(p))
        {
            out(&sub);
            walk(&sub, depth - 1, skip, out);
        }
    }
    walk(dir, 4, &skip, &mut push_dir);
    // The Engine tree is skipped above because it is full of helper exes, but
    // Satisfactory keeps its shipping exe in exactly one place inside it (#29).
    let eng = dir.join("Engine").join("Binaries").join("Win64");
    if eng.is_dir() {
        push_dir(&eng);
    }
    let folder = norm(dir.file_name().and_then(|n| n.to_str()).unwrap_or(""));
    let mut scored: Vec<(u8, i64, PathBuf)> = found
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let bits = exe_bitness(&p).ok()?;
            if is_helper_name(&stem) {
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
            // A 32-bit exe sitting beside a 64-bit one is usually a tool, so a
            // 64-bit candidate still outranks every 32-bit one. It used to
            // *delete* them, which hid the real game: Prince of Persia: The Two
            // Thrones keeps a 64-bit launcher named after the folder next to the
            // 32-bit game, and the game could not be chosen at all (#89). They
            // are ranked, not discarded, so the exe picker still offers them.
            if bits == 64 {
                score += 8_000_000_000;
            }
            Some((bits, score, p))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.into_iter().map(|(_, _, p)| p).collect()
}

/// Accepts either a game exe or a game folder; returns the exe to use plus
/// every candidate found.
///
/// When the input is a **file** (e.g. a 500 KB Unreal bootstrapper next to a
/// `Game\Binaries\Win64\*-Shipping.exe`), we still search the parent tree so
/// Install lands next to the real game — not the launcher (Ghostrunner).
pub fn resolve_target(input: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    if input.is_file() {
        // A named file is an instruction, not a hint: installing into a different
        // game than the one the user pointed at writes DLLs into the wrong folder.
        // The one substitution worth making is the Unreal launcher case, where
        // `…/Game/Game.exe` is a bootstrapper and the real target is the
        // `-Shipping.exe` under that same folder — never a sibling directory's game.
        let shipping = input
            .parent()
            .map(find_game_exes)
            .unwrap_or_default()
            .into_iter()
            .find(|p| {
                p != input
                    && p.file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.to_ascii_lowercase().ends_with("-shipping"))
            });
        return match shipping {
            Some(ship) => Ok((ship.clone(), vec![ship, input.to_path_buf()])),
            None => Ok((input.to_path_buf(), Vec::new())),
        };
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

/// True when ReShade/Feeder markers sit on a launcher folder but the preferred
/// Shipping exe's directory has neither — classic wrong-folder Install.
pub fn install_folder_mismatch(preferred_exe: &Path) -> Option<String> {
    let ship_dir = preferred_exe.parent()?;
    let ship_ok = ship_dir.join(RESHADE_PROXY).is_file() || ship_dir.join(FEEDER_ADDON).is_file();
    if ship_ok {
        return None;
    }
    // Walk up looking for a sibling/parent that has our install but is not ship_dir.
    let mut cur = ship_dir.parent();
    for _ in 0..4 {
        let Some(d) = cur else { break };
        if (d.join(RESHADE_PROXY).is_file() || d.join(FEEDER_MARKER).is_file()) && d != ship_dir {
            return Some(format!(
                "ReShade/Feeder found in {} but not next to {} — Install on the Shipping exe",
                d.display(),
                preferred_exe
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("game.exe")
            ));
        }
        cur = d.parent();
    }
    None
}

/// True when EffectSearchPaths point at a missing/empty Shaders folder.
pub fn shaders_missing(game_dir: &Path) -> bool {
    let sh = game_dir.join("reshade-shaders").join("Shaders");
    if !sh.is_dir() {
        return game_dir.join(RESHADE_PROXY).is_file();
    }
    fs::read_dir(&sh)
        .ok()
        .map(|rd| {
            !rd.flatten().any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("fx"))
            })
        })
        .unwrap_or(true)
        && game_dir.join(RESHADE_PROXY).is_file()
}

/// Sidecar markers this tool leaves so Install / Update / Remove know the folder.
pub fn installed_by_tool(dir: &Path) -> bool {
    [
        OPTI_MANIFEST,
        AIO_MANIFEST,
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

    /// A minimal PE32+ with one import descriptor, so the import reader can be
    /// tested on a file whose contents are known exactly.
    pub fn make_pe_importing(path: &Path, dll: &str, fns: &[&str]) -> PathBuf {
        make_pe_importing_many(path, &[(dll, fns)])
    }

    /// The same, with one descriptor per (DLL, functions) pair.
    pub fn make_pe_importing_many(path: &Path, dlls: &[(&str, &[&str])]) -> PathBuf {
        // One section mapped 1:1 (RVA == file offset) keeps the maths trivial.
        const SEC: usize = 0x400;
        let mut img = vec![0u8; SEC];
        img[..2].copy_from_slice(b"MZ");
        img[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        let pe = 0x80usize;
        img[pe..pe + 4].copy_from_slice(b"PE\0\0");
        img[pe + 4..pe + 6].copy_from_slice(&PE_X64.to_le_bytes());
        img[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes()); // 1 section
        let opt_size: u16 = 240;
        img[pe + 20..pe + 22].copy_from_slice(&opt_size.to_le_bytes());
        let opt = pe + 24;
        img[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes()); // PE32+

        let mut blob: Vec<u8> = Vec::new();
        let at = |b: &Vec<u8>| (SEC + b.len()) as u32;
        // Names and thunks first, one set per DLL; the descriptors follow.
        let mut parts: Vec<(u32, u32)> = Vec::new(); // (dll name RVA, thunk RVA)
        for (dll, fns) in dlls {
            let mut name_rvas = Vec::new();
            for f in *fns {
                name_rvas.push(at(&blob));
                blob.extend_from_slice(&0u16.to_le_bytes());
                blob.extend_from_slice(f.as_bytes());
                blob.push(0);
            }
            let dll_rva = at(&blob);
            blob.extend_from_slice(dll.as_bytes());
            blob.push(0);
            while !blob.len().is_multiple_of(8) {
                blob.push(0);
            }
            let thunk_rva = at(&blob);
            for r in &name_rvas {
                blob.extend_from_slice(&(*r as u64).to_le_bytes());
            }
            blob.extend_from_slice(&0u64.to_le_bytes()); // terminator
            parts.push((dll_rva, thunk_rva));
        }
        let desc_rva = at(&blob);
        for (dll_rva, thunk_rva) in &parts {
            blob.extend_from_slice(&thunk_rva.to_le_bytes()); // OriginalFirstThunk
            blob.extend_from_slice(&[0u8; 8]); // TimeDateStamp, ForwarderChain
            blob.extend_from_slice(&dll_rva.to_le_bytes()); // Name
            blob.extend_from_slice(&thunk_rva.to_le_bytes()); // FirstThunk
        }
        blob.extend_from_slice(&[0u8; 20]); // null descriptor

        // PE32+ data directories start at optional-header offset 112, and the
        // import table is index 1, so it sits eight bytes further in.
        img[opt + 120..opt + 124].copy_from_slice(&desc_rva.to_le_bytes());
        // Section header: VirtualAddress == PointerToRawData == SEC.
        let sh = opt + opt_size as usize;
        img[sh + 8..sh + 12].copy_from_slice(&(blob.len() as u32).to_le_bytes()); // VirtualSize
        img[sh + 12..sh + 16].copy_from_slice(&(SEC as u32).to_le_bytes()); // VirtualAddress
        img[sh + 16..sh + 20].copy_from_slice(&(blob.len() as u32).to_le_bytes()); // SizeOfRawData
        img[sh + 20..sh + 24].copy_from_slice(&(SEC as u32).to_le_bytes()); // PointerToRawData
        img.extend_from_slice(&blob);
        fs::write(path, &img).unwrap();
        path.to_path_buf()
    }

    /// Minimal PE with a real import directory so `pe_imports` / `detect_api`
    /// can see sibling engine DLLs the way Gothic 3 does (exe → Engine.dll → d3d9).
    pub fn make_pe_with_imports(path: &Path, machine: u16, dlls: &[&str]) -> PathBuf {
        let pe32 = machine == PE_X86;
        let opt_magic: u16 = if pe32 { 0x10B } else { 0x20B };
        // Standard optional header sizes including 16 data directories.
        let opt_size: u16 = if pe32 { 224 } else { 240 };
        let sect_raw = 0x400usize;
        let sect_va = 0x1000usize;

        let mut names_blob = Vec::new();
        let mut name_rvas = Vec::new();
        // Descriptors first (20 bytes each + null), then the DLL name strings.
        let desc_bytes = (dlls.len() + 1) * 20;
        for dll in dlls {
            name_rvas.push(sect_va + desc_bytes + names_blob.len());
            names_blob.extend_from_slice(dll.as_bytes());
            names_blob.push(0);
        }
        let mut idata = vec![0u8; desc_bytes];
        for (i, &rva) in name_rvas.iter().enumerate() {
            let off = i * 20;
            idata[off + 12..off + 16].copy_from_slice(&(rva as u32).to_le_bytes());
        }
        idata.extend_from_slice(&names_blob);
        while !idata.len().is_multiple_of(16) {
            idata.push(0);
        }

        let mut file = vec![0u8; sect_raw + idata.len()];
        file[0] = b'M';
        file[1] = b'Z';
        file[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());

        let pe = 0x80usize;
        file[pe..pe + 4].copy_from_slice(b"PE\0\0");
        file[pe + 4..pe + 6].copy_from_slice(&machine.to_le_bytes());
        file[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes()); // NumberOfSections
        file[pe + 20..pe + 22].copy_from_slice(&opt_size.to_le_bytes());

        let opt = pe + 24;
        file[opt..opt + 2].copy_from_slice(&opt_magic.to_le_bytes());
        // DataDirectory[1] = Import (Export is [0] at +0/+4; Import follows at +8).
        let dd_base = opt + if pe32 { 96 } else { 112 };
        let n_dirs = dd_base - 4; // NumberOfRvaAndSizes sits just before the dirs
        file[n_dirs..n_dirs + 4].copy_from_slice(&16u32.to_le_bytes());
        file[dd_base + 8..dd_base + 12].copy_from_slice(&(sect_va as u32).to_le_bytes());
        file[dd_base + 12..dd_base + 16].copy_from_slice(&(idata.len() as u32).to_le_bytes());

        let sec = opt + opt_size as usize;
        file[sec..sec + 8].copy_from_slice(b".idata\0\0");
        file[sec + 8..sec + 12].copy_from_slice(&(idata.len() as u32).to_le_bytes()); // VirtualSize
        file[sec + 12..sec + 16].copy_from_slice(&(sect_va as u32).to_le_bytes());
        file[sec + 16..sec + 20].copy_from_slice(&(idata.len() as u32).to_le_bytes()); // SizeOfRawData
        file[sec + 20..sec + 24].copy_from_slice(&(sect_raw as u32).to_le_bytes());

        file[sect_raw..sect_raw + idata.len()].copy_from_slice(&idata);
        fs::write(path, file).unwrap();
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

    /// Max Payne is a 32-bit game and has no 64-bit exe at all; picking the
    /// folder used to find nothing, so it only worked when the exe was chosen
    /// by hand (#17). The feeder drives 32-bit games through its host64 helper.
    /// After one install a 32-bit game folder contains host64\dlss5-feed-host64.exe,
    /// which is a 64-bit PE. The scan ranked it above the real 32-bit game exe,
    /// so the next Install wrote a 64-bit ReShade into a 32-bit game's folder and
    /// the game silently stopped loading it (#69).
    #[test]
    fn our_own_host_helper_is_never_the_game() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Grand Theft Auto IV");
        let host = d.join(HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        let game = make_pe(&d.join("GTAIV.exe"), PE_X86);
        make_pe(&host.join(HOST_EXE), PE_X64);

        let found = find_game_exes(&d);
        assert_eq!(found, vec![game.clone()], "{found:?}");
        let (exe, _) = resolve_target(&d).unwrap();
        assert_eq!(exe, game);
        assert_eq!(exe_bitness(&exe).unwrap(), 32);
    }

    #[test]
    fn find_game_exes_falls_back_to_32bit_when_no_64bit_exists() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Max Payne");
        fs::create_dir_all(&d).unwrap();
        make_pe(&d.join("maxpayne.exe"), PE_X86);
        assert_eq!(find_game_exes(&d), vec![d.join("maxpayne.exe")]);
    }

    /// Prince of Persia: The Two Thrones ships a 64-bit launcher named after
    /// the folder beside the 32-bit game. The 64-bit one still wins by default,
    /// but the game must stay in the list or it cannot be chosen at all (#89).
    #[test]
    fn a_thirty_two_bit_game_stays_listed_behind_a_64_bit_launcher() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Prince of Persia Two Thrones");
        fs::create_dir_all(&d).unwrap();
        let launcher = d.join("PrinceOfPersia.exe");
        let game = d.join("PoP3.exe");
        make_pe(&launcher, PE_X64);
        make_pe(&game, PE_X86);
        let found = find_game_exes(&d);
        assert_eq!(found.first(), Some(&launcher), "{found:?}");
        assert!(found.contains(&game), "{found:?}");
    }

    /// A named .exe is an instruction. Searching outward from it and picking a
    /// "better" candidate installed into a different game entirely — the tool
    /// wrote to a sibling folder's exe when handed one under a shared parent.
    #[test]
    fn resolve_target_honours_a_named_exe() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("GameA");
        let b = t.path().join("GameB");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let mine = make_pe(&a.join("small.exe"), PE_X64);
        // A much larger exe next door would outrank it on size.
        let big = b.join("Huge.exe");
        make_pe(&big, PE_X64);
        fs::write(&big, [b"MZ".as_slice(), &[0u8; 8_000_000]].concat()).unwrap();
        make_pe(&big, PE_X64);
        let (exe, _) = resolve_target(&mine).unwrap();
        assert_eq!(exe, mine);
    }

    /// The one substitution that is still right: an Unreal launcher beside its
    /// own -Shipping.exe resolves to the shipping build.
    #[test]
    fn resolve_target_prefers_shipping_beside_a_launcher() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("SomeGame");
        fs::create_dir_all(&d).unwrap();
        let launcher = make_pe(&d.join("SomeGame.exe"), PE_X64);
        let ship = make_pe(&d.join("SomeGame-Win64-Shipping.exe"), PE_X64);
        let (exe, all) = resolve_target(&launcher).unwrap();
        assert_eq!(exe, ship);
        assert!(all.contains(&launcher));
    }

    /// Madden NFL 27 ships EA Javelin, which is kernel-mode and — by EA's own
    /// description — guards single-player too. NGX then refuses to initialise in
    /// the game's process (0xBAD00001 on the capability query itself), and the
    /// tool said nothing about it, so the reporter spent days on reinstalls (#21).
    #[test]
    fn ea_javelin_is_detected_by_its_launcher_and_folder() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert_eq!(detect_anticheat(d), None);

        fs::write(d.join("EAAntiCheat.GameServiceLauncher.exe"), b"x").unwrap();
        assert_eq!(detect_anticheat(d), Some("EA Javelin Anticheat"));

        let t2 = tempfile::tempdir().unwrap();
        fs::create_dir_all(t2.path().join("EAAntiCheat")).unwrap();
        assert_eq!(detect_anticheat(t2.path()), Some("EA Javelin Anticheat"));
    }

    /// Blizzard blocked a dxgi.dll override in retail WoW twice in 2025 and
    /// treats render-chain injection as a terms violation. Installing into it
    /// silently is the one outcome worth refusing outright (#45).
    #[test]
    fn world_of_warcraft_is_named_before_anything_is_installed() {
        for n in ["Wow.exe", "WowClassic.exe", "wow.exe"] {
            assert_eq!(
                known_anticheat_exe(Path::new(n)),
                Some("Blizzard Warden (World of Warcraft)"),
                "{n}"
            );
        }
        assert_eq!(known_anticheat_exe(Path::new("wowzers.exe")), None);
    }

    /// dinput8.dll is a proxy slot, not a product. The RTX 20/30 frame-gen mods
    /// ship their own, and an RE Engine game with one installed looked like it
    /// already had REFramework — so Install skipped it, and RE Engine games
    /// crash under ReShade without it.
    #[test]
    fn someone_elses_dinput8_is_not_reframework() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let dll = d.join(REFRAMEWORK_DLL);
        assert!(!is_reframework_dll(&dll), "missing file is not REFramework");

        // What the frame-generation proxies look like: nvapi interception, no
        // mention of REFramework or its author.
        fs::write(
            &dll,
            b"MZ\x00\x00nvapi_QueryInterface\x00sm_75\x00NVAPI_GPU_ARCHITECTURE_AD100\x00",
        )
        .unwrap();
        assert!(
            !is_reframework_dll(&dll),
            "a frame-gen proxy is not REFramework"
        );

        fs::write(&dll, b"MZ\x00\x00REFramework\x00praydog\x00").unwrap();
        assert!(is_reframework_dll(&dll));
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
        // The helper is dropped by name; the 32-bit tool is ranked last rather
        // than hidden, because hiding every 32-bit exe hid real games (#89).
        assert_eq!(c.first(), Some(&d.join("Fell & Sell.exe")), "{c:?}");
        assert!(!c.contains(&d.join("UnityCrashHandler64.exe")), "{c:?}");
        let (exe, _all) = resolve_target(&d).unwrap();
        assert_eq!(exe, d.join("Fell & Sell.exe"));
    }

    /// Two launchers that outranked the game: Ubisoft's
    /// Support\GDF\FirewallInstall.exe beat ACBSP.exe (#105), and Rockstar's
    /// 64-bit PlayGTAIV.exe beat the 32-bit GTAIV.exe (#94).
    #[test]
    fn firewall_installers_and_playgtaiv_are_not_the_game() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Assassin's Creed Brotherhood");
        fs::create_dir_all(d.join("Support").join("GDF")).unwrap();
        make_pe(&d.join("ACBSP.exe"), PE_X86);
        make_pe(
            &d.join("Support").join("GDF").join("FirewallInstall.exe"),
            PE_X64,
        );
        let (exe, _) = resolve_target(&d).unwrap();
        assert_eq!(exe, d.join("ACBSP.exe"));

        let g = t.path().join("Grand Theft Auto IV").join("GTAIV");
        fs::create_dir_all(&g).unwrap();
        make_pe(&g.join("GTAIV.exe"), PE_X86);
        make_pe(&g.join("PlayGTAIV.exe"), PE_X64);
        let (exe, _) = resolve_target(&g).unwrap();
        assert_eq!(exe, g.join("GTAIV.exe"));
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
    /// Red Dead Redemption 2 imports d3d9.dll only for the D3DPERF debug
    /// markers and renders with something far newer, so v0.11.17's renderer
    /// scan read it as a DirectX 9 game and refused to install (#53).
    #[test]
    fn d3d9_debug_markers_alone_are_not_a_d3d9_game() {
        let t = tempfile::tempdir().unwrap();
        let markers = testutil::make_pe_importing(
            &t.path().join("markers.exe"),
            "d3d9.dll",
            &["D3DPERF_BeginEvent", "D3DPERF_EndEvent"],
        );
        assert_eq!(
            pe_import_fns(&markers, "d3d9.dll"),
            vec!["d3dperf_beginevent", "d3dperf_endevent"]
        );
        assert_eq!(pe_imports(&markers), vec!["d3d9.dll"]);
        assert_eq!(detect_api(&markers), Api::Unknown);

        // A real Direct3D 9 renderer still reads as one.
        let real = testutil::make_pe_importing(
            &t.path().join("real.exe"),
            "d3d9.dll",
            &["Direct3DCreate9", "D3DPERF_BeginEvent"],
        );
        assert_eq!(detect_api(&real), Api::Dx9);

        // Direct3D 9 predates DXGI, so a PE importing both is not a D3D9 game
        // however it uses d3d9.dll - it creates its real device at runtime.
        let dxgi = testutil::make_pe_importing_many(
            &t.path().join("dxgi.exe"),
            &[
                ("d3d9.dll", &["Direct3DCreate9"][..]),
                ("dxgi.dll", &["CreateDXGIFactory1"][..]),
            ],
        );
        assert_eq!(detect_api(&dxgi), Api::Unknown);

        // Red Dead Redemption 2's own import table (#53): a real
        // Direct3DCreate9Ex import, no dxgi.dll at all, and the FSR2 and Reflex
        // libraries for D3D12 and Vulkan beside it.
        let rdr2 = testutil::make_pe_importing_many(
            &t.path().join("RDR2.exe"),
            &[
                ("d3d9.dll", &["Direct3DCreate9Ex"][..]),
                ("nvlowlatencyvk.dll", &["NvLL_VK_Initialize"][..]),
                (
                    "ffx_fsr2_api_dx12_x64.dll",
                    &["ffxFsr2GetInterfaceDX12"][..],
                ),
                ("ffx_fsr2_api_vk_x64.dll", &["ffxFsr2GetInterfaceVK"][..]),
            ],
        );
        assert_eq!(detect_api(&rdr2), Api::Unknown);
    }

    #[test]
    fn classify_imports_reads_d3d9() {
        // Aion imports d3d9.dll (through XRenderD3D9.dll) and nothing newer;
        // before this it read as "unknown, assume DX12" and the tool installed
        // a dxgi.dll the game never loads (#16).
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(classify_imports(&s(&["d3d9.dll"])), Api::Dx9);
        // A game that also uses something newer is not a D3D9 game.
        assert_eq!(classify_imports(&s(&["d3d9.dll", "d3d11.dll"])), Api::Dx11);
        assert_eq!(classify_imports(&s(&["d3d9.dll", "d3d12.dll"])), Api::Dx12);
    }

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
        assert_eq!(classify_imports(&s(&["d3d9.dll"])), Api::Dx9);
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
    fn find_game_exes_accepts_bin_launchers() {
        // Aion's launcher is aion.bin, an ordinary PE with an unusual
        // extension; before this it was invisible to the scan (#16).
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Aion");
        fs::create_dir_all(&d).unwrap();
        let exe = d.join("aion.bin");
        make_pe(&exe, PE_X64);
        assert_eq!(find_game_exes(&d), vec![exe]);

        // A .bin that is not a PE at all is still ignored.
        let t2 = tempfile::tempdir().unwrap();
        let d2 = t2.path().join("Game");
        fs::create_dir_all(&d2).unwrap();
        fs::write(d2.join("data.bin"), b"not a PE, just data").unwrap();
        assert!(find_game_exes(&d2).is_empty());
    }

    #[test]
    fn find_game_exes_finds_exe_four_levels_down() {
        // Dying Light: The Beast keeps its exe at ph_ft\work\bin\x64 (#43);
        // two levels of search reported "no 64-bit game executable found".
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Dying Light The Beast");
        let deep = d.join("ph_ft").join("work").join("bin").join("x64");
        fs::create_dir_all(&deep).unwrap();
        let exe = deep.join("DyingLightGame_TheBeast_x64_rwdi.exe");
        make_pe(&exe, PE_X64);
        assert_eq!(find_game_exes(&d), vec![exe]);

        // Five levels down is still out of reach, and content trees stay skipped.
        let t2 = tempfile::tempdir().unwrap();
        let d2 = t2.path().join("Game");
        let too_deep = d2.join("a").join("b").join("c").join("d").join("e");
        fs::create_dir_all(&too_deep).unwrap();
        make_pe(&too_deep.join("Game.exe"), PE_X64);
        let content = d2.join("Content").join("bin");
        fs::create_dir_all(&content).unwrap();
        make_pe(&content.join("Game.exe"), PE_X64);
        assert!(find_game_exes(&d2).is_empty());
    }

    #[test]
    fn resolve_target_launcher_file_prefers_shipping() {
        // Ghostrunner-style: root launcher + Shipping under Game\Binaries\Win64.
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Ghostrunner");
        let bin = d.join("Ghostrunner").join("Binaries").join("Win64");
        fs::create_dir_all(&bin).unwrap();
        let launcher = make_pe(&d.join("Ghostrunner.exe"), PE_X64);
        let shipping = make_pe(&bin.join("Ghostrunner-Win64-Shipping.exe"), PE_X64);
        // Passing the launcher FILE must still resolve to Shipping.
        let (exe, all) = resolve_target(&launcher).unwrap();
        assert_eq!(exe, shipping);
        assert!(all.contains(&shipping));
        assert!(all.contains(&launcher));
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

    /// Gothic 3: the exe has no Direct3D imports; Engine.dll does (d3d9.dll).
    /// Sibling scan must surface Dx9; Install is allowed and will fetch dgVoodoo.
    #[test]
    fn detect_api_reads_dx9_from_sibling_engine_dll() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe_with_imports(&d.join("Gothic3.exe"), PE_X86, &["engine.dll"]);
        make_pe_with_imports(
            &d.join("Engine.dll"),
            PE_X86,
            &["d3d9.dll", "d3dx9_40.dll", "kernel32.dll"],
        );
        assert_eq!(pe_imports(&exe), vec!["engine.dll".to_string()]);
        assert!(pe_imports(&d.join("Engine.dll")).contains(&"d3d9.dll".to_string()));
        assert_eq!(detect_api(&exe), Api::Dx9);

        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = inspect(&exe).unwrap();
        assert_eq!(st.api, Api::Dx9);
        assert!(
            st.needs_dgvoodoo(),
            "DX9 without dgVoodoo should need the install step"
        );
        assert!(
            !st.problems.iter().any(|p| p.contains("DirectX 9")),
            "DX9 without dgVoodoo must not hard-block Install: {:?}",
            st.problems
        );

        fs::write(
            d.join("dgVoodoo.conf"),
            b"[General]\nOutputAPI = bestavailable\n",
        )
        .unwrap();
        let st = inspect(&exe).unwrap();
        assert_eq!(st.api, Api::Dx9);
        assert!(!st.needs_dgvoodoo());
        assert!(
            !st.problems.iter().any(|p| p.contains("DirectX 9")),
            "dgVoodoo present should stay clear: {:?}",
            st.problems
        );
    }

    /// Live check against a local Gothic 3 install when present.
    #[test]
    fn gothic3_live_detects_dx9_and_allows_install_without_dgvoodoo() {
        let exe = std::path::Path::new(r"D:\Games\Gothic 3\Gothic3.exe");
        if !exe.is_file() {
            return;
        }
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        assert_eq!(
            detect_api(exe),
            Api::Dx9,
            "Gothic3.exe must classify as DX9"
        );
        assert_eq!(detect_api(exe).label(), "DX9");
        let st = inspect(exe).unwrap();
        assert_eq!(st.api, Api::Dx9);
        assert_eq!(st.bitness, 32);
        let has_dg = is_dgvoodoo(exe.parent().unwrap());
        assert_eq!(st.needs_dgvoodoo(), !has_dg);
        assert!(
            !st.problems.iter().any(|p| p.contains("DirectX 9")),
            "Gothic 3 must not hard-block on missing dgVoodoo: {:?}",
            st.problems
        );
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
    fn dx9_needs_dgvoodoo_helper() {
        assert_eq!(classify_imports(&["d3d9.dll".into()]), Api::Dx9);
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert!(!is_dgvoodoo(d));
        fs::write(
            d.join("dgVoodoo.conf"),
            b"[General]\nOutputAPI = bestavailable\n",
        )
        .unwrap();
        assert!(is_dgvoodoo(d));
    }

    /// A renamed old ReShade (`d3d9.dll.off`) must not count as dgVoodoo.
    #[test]
    fn d3d9_off_is_not_dgvoodoo() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::write(d.join("d3d9.dll.off"), b"MZ old reshade").unwrap();
        assert!(!is_dgvoodoo(d));
    }

    #[test]
    fn dll_mentions_dgvoodoo_ascii_and_utf16() {
        assert!(dll_mentions_dgvoodoo(b"MZ...dgVoodoo2 wrapper..."));
        assert!(!dll_mentions_dgvoodoo(b"MZ some other wrapper"));
        // Official MS/x86/D3D9.dll (2.87.3 and 2.87.5) embeds the name as UTF-16LE only.
        let mut utf16 = b"MZ\0\0".to_vec();
        for &c in b"dgVoodoo" {
            utf16.push(c);
            utf16.push(0);
        }
        assert!(dll_mentions_dgvoodoo(&utf16));
    }

    /// Remote Desktop hides the real card behind a virtual adapter, so the GPU
    /// check refuses a machine that works locally. The escape hatch has to be
    /// reachable from the GUI, not only from an environment variable a running
    /// window cannot be given (#3).
    #[test]
    fn skip_gpu_check_is_the_same_switch_from_either_side() {
        std::env::remove_var(SKIP_GPU_CHECK_ENV);
        assert!(!skip_gpu_check());
        set_skip_gpu_check(true);
        assert!(skip_gpu_check());
        assert_eq!(std::env::var(SKIP_GPU_CHECK_ENV).as_deref(), Ok("1"));
        set_skip_gpu_check(false);
        assert!(!skip_gpu_check());
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
        fs::remove_file(d.join("Foo_BE.exe")).unwrap();
        // Arknights: Endfield's layout (#113).
        fs::create_dir_all(d.join("AntiCheatExpert")).unwrap();
        assert_eq!(detect_anticheat(d), Some("Anti-Cheat Expert (ACE)"));
    }

    /// Neural Upstream stands in for the RenoDX add-on, so an install that has
    /// it plus the model is complete without `renodx-dlss5.addon64` (#50).
    #[test]
    fn upstream_counts_as_the_neural_consumer() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), PE_X64);
        let mut st = inspect(&exe).unwrap();
        st.mode = Mode::Native;
        st.api = Api::Dx12;
        st.reshade = true;
        st.dlssnr = true;
        assert!(!st.complete());
        st.upstream = true;
        assert!(st.complete());
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

    #[test]
    fn unity_and_rt_likely_heuristics() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), PE_X64);
        assert!(!unity_likely(d));
        assert!(!rt_likely(d));
        fs::write(d.join("UnityPlayer.dll"), b"MZ").unwrap();
        assert!(unity_likely(d));
        fs::write(d.join("nvngx_dlssd.dll"), b"x").unwrap();
        assert!(rt_likely(d));
        let st = {
            std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
            inspect(&exe).unwrap()
        };
        assert!(st.unity_likely);
        assert!(st.rt_likely);
    }

    #[test]
    fn unreal_likely_from_binaries_layout() {
        let t = tempfile::tempdir().unwrap();
        let win64 = t.path().join("MyGame").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        let exe = make_pe(&win64.join("MyGame-Win64-Shipping.exe"), PE_X64);
        assert!(unreal_likely(&exe, &win64));
    }

    /// Game Pass installs are protected end to end; the scan finds the
    /// gamelaunchhelper.exe stub and read every one as "no DLSS, Feeder path".
    /// A MicrosoftGame.config above the exe is the tell, and whether the
    /// game ships DLSS is read from its folder, not from the locked exe.
    #[test]
    fn game_pass_copies_are_refused_only_when_the_folder_is_locked() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        // A writable folder with MicrosoftGame.config (the Steam copy of Forza
        // Horizon 6 ships one, and so do unprotected Store installs) inspects
        // like any other game.
        let t = tempfile::tempdir().unwrap();
        let content = t.path().join("Content");
        fs::create_dir_all(&content).unwrap();
        fs::write(content.join("MicrosoftGame.config"), "<Game/>").unwrap();
        let exe = testutil::make_pe(&content.join("ForzaHorizon6.exe"), PE_X64);
        assert!(inspect(&exe).is_ok());
        assert!(!content.join(".dlss5oneclick-write-probe").exists());
        assert!(!dir_writable(Path::new(
            r"C:\Windows\System32\drivers\etc\nonexistent-dir"
        )));
    }

    /// A folder of thousands of entries is skipped by every walk; the exe,
    /// the DLLs and the anti-cheat markers beside it are still found.
    #[test]
    fn huge_folders_are_not_walked() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let dump = d.join("paz_decrypted");
        fs::create_dir_all(dump.join("deep")).unwrap();
        for i in 0..2100 {
            fs::write(dump.join(format!("{i}.bin")), b"x").unwrap();
        }
        assert!(huge_dir(&dump));
        assert!(!huge_dir(d));
        // Markers hidden inside the dump are not seen; beside the exe they are.
        fs::write(dump.join("deep").join(DLSS_DLL), b"x").unwrap();
        assert!(!game_ships_dlss(d));
        fs::write(d.join(DLSS_DLL), b"x").unwrap();
        assert!(game_ships_dlss(d));
    }
}
