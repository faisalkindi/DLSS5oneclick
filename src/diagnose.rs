//! Read the logs a session leaves behind and say why neural rendering is or is
//! not running. Answers the commonest report ("I enabled it, nothing changed")
//! without a round trip: everything needed is already in `ReShade.log` and,
//! on the Feeder path, `dlss5-feed.log` next to the game exe.

use crate::game::{self, GameStatus};
use anyhow::Result;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub level: Level,
    pub text: String,
}

/// `NVSDK_NGX_*_Init -> 0xBAD00001` in a log: NGX itself refused. Add what the
/// system says about NGX Core, which is the usual cause on capable hardware.
/// `exe` is the game (and, for a 32-bit game, its helper) whose Windows GPU
/// preference is worth naming: on a hybrid machine a process started on the
/// iGPU gets exactly this error, because NGX does not exist there (#25).
fn ngx_init_failure_for(log: &str, exe: Option<&std::path::Path>, out: &mut Vec<Finding>) {
    let Some(line) = log
        .lines()
        .find(|l| l.contains("NVSDK_NGX") && l.contains("Init") && l.contains("0xBAD00001"))
    else {
        return;
    };
    if crate::gpupref::hybrid() {
        let set = exe.is_some_and(|e| {
            crate::gpupref::get(e).is_some_and(|v| crate::gpupref::is_high_performance(&v))
        });
        let names = crate::gpupref::real_adapters();
        out.push(bad(format!(
            "More than one GPU vendor on this machine ({}), and Windows decides which one              a process starts on. Started on the integrated GPU, NGX does not exist and              every Init answers 0xBAD00001 — this is the most likely cause here{}.              Settings ▸ System ▸ Display ▸ Graphics ▸ Add a desktop app ▸ pick the game's exe              (and, for a 32-bit game, host64\\dlss5-feed-host64.exe) ▸ Options ▸ High performance.              Install sets that for you from this version on.",
            names.join(", "),
            if set {
                ", though the preference is already set to high performance for that exe"
            } else {
                ""
            }
        )));
    }
    let system = crate::ngx::describe();
    // Reported on three machines (RTX 4070, 5080, 5090) with NGX Core present and
    // driver 616.56, always on the Feeder's own in-process D3D12 device. The same
    // chain initialises NGX fine in the 32-bit host64 helper (a separate process)
    // and on the native path where the game owns the device, so the installed
    // files are not what decides it.
    let advice = if crate::ngx::healthy() {
        "Your NGX runtime and driver are fine, so this is NGX refusing the Feeder's private          D3D12 device inside the game process, which has been reported on several machines.          Worth doing: install into a game that ships its own DLSS (that path opens no private          device) to confirm NGX works for you, then report this log at          github.com/jlrouzies-fr/DLSS5-Feeder, where that device is created."
    } else {
        "Fix that first, then run Install again: reinstall the NVIDIA driver with a Custom          install that keeps every component (616.56 or newer)."
    };
    out.push(bad(format!(
        "NGX refused to initialise: {}. 0xBAD00001 is FeatureNotSupported, which NGX also          answers when its runtime is not on the system — not a ReShade, shader or add-on          problem. {system}. {advice}",
        line.trim()
    )));
}

/// Newest Feeder known at build time; only used to nudge users off stale copies.
const CURRENT_FEEDER: &str = "0.15.1";

fn version_key(v: &str) -> Vec<u64> {
    v.split(['.', '-'])
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

fn ok(t: impl Into<String>) -> Finding {
    Finding {
        level: Level::Ok,
        text: t.into(),
    }
}
fn warn(t: impl Into<String>) -> Finding {
    Finding {
        level: Level::Warn,
        text: t.into(),
    }
}
fn bad(t: impl Into<String>) -> Finding {
    Finding {
        level: Level::Bad,
        text: t.into(),
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok()
}

/// The exe ReShade actually loaded into, from its first line:
/// `... loaded from '...dxgi.dll' into 'C:\\...bg3_dx11.exe' (0x...)`.
fn reshade_host_exe(log: &str) -> Option<String> {
    let line = log
        .lines()
        .find(|l| l.contains("loaded from") && l.contains(" into "))?;
    let path = line.split(" into ").nth(1)?.split('\'').nth(1)?;
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Findings for a game folder, in reading order.
pub fn diagnose(st: &GameStatus) -> Vec<Finding> {
    let d = st.game_dir();
    let mut out = Vec::new();
    let consumer = st.consumer_dir();
    let rs_log = read(&consumer, "ReShade.log").or_else(|| read(&consumer, "ReShade2.log"));
    // Wine and Proton substitute their own d3dcompiler_47.dll, whose HLSL
    // compiler is vkd3d-shader. It does not implement every attribute ReShade
    // emits, and says so in its own words (#70).
    let wine_hlsl = rs_log
        .as_deref()
        .is_some_and(|l| l.contains("not yet implemented feature"));

    // ── a game-shipped HLSL compiler shadowing the system one ──────
    // The add-on compiles its NR pass at cs_5_1. A d3dcompiler_47.dll that
    // ships with the game is loaded in preference to System32's, and an old
    // one does not know that target: "error X3506: unrecognized compiler
    // target" and no neural rendering, with everything else looking correct.
    let compiler = d.join("d3dcompiler_47.dll");
    if compiler.is_file() && !wine_hlsl {
        let ver = crate::ngx::file_version(&compiler).unwrap_or_else(|| "unknown".into());
        out.push(warn(format!(
            "The game ships its own d3dcompiler_47.dll ({ver}), which Windows loads instead of \
             System32's. If it predates shader model 5.1 the DLSS 5 pass cannot compile \
             (error X3506). Rename it to d3dcompiler_47.dll.bak and start the game again; \
             almost every game runs fine on the system copy. On Wine/Proton, leave it: a \
             copy of Microsoft's compiler beside the game, loaded through a WINEDLLOVERRIDES \
             entry, is what got DLSS 5 compiling there at all (#76)."
        )));
    }

    // ── dgVoodoo's reported VRAM vs what the game was told ─────────
    // A game that reads VRAM from the adapter and sizes its own pools from it
    // has to be told the same number, or it sizes for one figure and allocates
    // against another. GTA IV is the case in hand: with dgVoodoo reporting
    // 4096 MB and no matching -availablevidmem it black-screens after load,
    // and with no conf at all it takes dgVoodoo's stock 256 MB and dies with
    // "TEXP60: Unable to create color render target" (#69).
    let conf = d.join("dgVoodoo.conf");
    if conf.is_file() {
        let vram = fs::read_to_string(&conf).ok().and_then(|t| {
            t.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("VRAM"))
                .and_then(|(_, v)| v.trim().trim_end_matches("MB").trim().parse::<u32>().ok())
        });
        let cmdline = d.join("commandline.txt");
        if let (Some(vram), true) = (vram, cmdline.is_file()) {
            let text = fs::read_to_string(&cmdline).unwrap_or_default();
            if !text.to_ascii_lowercase().contains("-availablevidmem") {
                out.push(warn(format!(
                    "dgVoodoo reports {vram} MB of video memory and this game reads that number \
                     to size its own memory pools, but commandline.txt does not set \
                     -availablevidmem. Add \"-availablevidmem {}\" to commandline.txt — slightly \
                     below the dgVoodoo figure on purpose, which is what the dgVoodoo guides for \
                     this engine call for. Without it the game sizes for one number and allocates \
                     against another, which shows up as a black screen after loading or as \
                     TEXP60 / TEXP70 at startup.",
                    vram.saturating_sub(64).max(256)
                )));
            }
        }
    }

    // ── which neural model is installed ────────────────────
    // Two builds of nvngx_dlssnr.dll are in circulation and only the version
    // resource separates them; every failing RTX 50 report so far carries the
    // .SF one, so the log has to name it.
    for p in [d.join(game::DLSSNR_DLL), consumer.join(game::DLSSNR_DLL)] {
        if !p.is_file() {
            continue;
        }
        if let Some(v) = crate::ngx::file_version(&p) {
            out.push(ok(format!(
                "DLSS 5 model {}: {v} — {}",
                if p.parent() == Some(d) {
                    "beside the exe"
                } else {
                    "in host64"
                },
                crate::ngx::model_build(&v)
            )));
        }
        break;
    }

    // ── ReShade side ────────────────────────────────────────────────
    // The DLSS 5 add-on runs under the ReShade in `consumer_dir()`: beside the
    // exe for a 64-bit game, in host64\ for a 32-bit one. Reading the game
    // folder's log for a 32-bit game reads the *feeder's* 32-bit ReShade, which
    // never loads the add-on, so every 32-bit report came back "the add-on
    // never registered" no matter how healthy the install was (#69).
    // A 32-bit game has two ReShades: the one beside the exe, which the Home key
    // opens and which loads the feeder, and the 64-bit one in host64\ that hosts
    // the neural add-on. Reporting only the second leaves "Home does nothing"
    // unexplained, which is the first thing the player actually notices (#69).
    if st.is32() && !game::is_reshade_dll(&d.join(game::RESHADE_PROXY)) {
        out.push(bad(format!(
            "No ReShade beside the game exe: {} is missing or is not ReShade, so the Home key \
             opens nothing and the feeder never loads. That is upstream of anything in host64\\. \
             Run Remove and then Install again, and if it comes back missing, check antivirus \
             history for {}.",
            game::RESHADE_PROXY,
            game::RESHADE_PROXY
        )));
    }
    let Some(rs) = rs_log else {
        out.push(bad(if st.is32() {
            "No host64\\ReShade.log: the 64-bit helper's ReShade never loaded, which is what \
             \"host lost: pipe never appeared\" in dlss5-feed.log means. Look in \
             host64\\dlss5-feed-host.log for the reason, and check antivirus did not remove \
             anything from host64\\."
        } else {
            "No ReShade.log next to the game exe: ReShade never loaded. Either the game was not \
             started since the install, or it does not load dxgi.dll (wrong exe picked, or a \
             launcher starts a different one). Check the exe with --check."
        }));
        return out;
    };
    if rs.contains("Initializing crosire's ReShade") {
        out.push(ok("ReShade loaded into the game."));
    }
    // Neural Upstream stands in for the RenoDX add-on and logs under its own
    // tag. Reading its log for the RenoDX add-on's lines reported "the add-on
    // never registered" and "no NGX call was intercepted" on an install whose
    // log showed the add-on registered and hooked (#86).
    if st.upstream {
        out.extend(upstream_findings(&rs));
        return out;
    }
    let failed_line = rs
        .lines()
        .find(|l| l.contains("Failed to load add-on") && l.contains("renodx-dlss5"));
    if let Some(l) = failed_line {
        let code = l
            .rsplit("error code ")
            .next()
            .unwrap_or("")
            .trim_end_matches('!');
        let extra = match code.trim() {
            "2148073478" => " (0x80090006 = the process refuses unsigned DLLs; nothing can be done)",
            "1114" => " (the add-on's DLL entry point failed; usually a CPU without AVX2 or a mismatched ReShade version)",
            _ => "",
        };
        out.push(bad(format!(
            "ReShade refused to load renodx-dlss5.addon64, error code {code}{extra}."
        )));
    } else if rs.contains("DLSS 5 Neural Rendering") {
        out.push(ok("The DLSS 5 Neural Rendering add-on registered."));
    } else {
        out.push(bad(format!(
            "The DLSS 5 add-on never registered. renodx-dlss5.addon64 is missing from {}, \
             disabled in ReShade's Add-ons tab, or quarantined by antivirus.",
            if st.is32() {
                "host64\\ (where a 32-bit game's add-on lives)"
            } else {
                "the game folder"
            }
        )));
    }
    if rs.contains("NR toggled ON") && !rs.contains("NR toggled OFF") {
        out.push(ok("Neural rendering was toggled ON (F6)."));
    } else if rs.contains("NR toggled OFF") {
        out.push(warn(
            "The log's last F6 state may be OFF — press F6 in game and watch the add-on's panel.",
        ));
    }
    if rs.contains("inline feature 18 evaluation succeeded") {
        out.push(ok(
            "Neural rendering ran: the add-on evaluated the DLSS 5 model on real frames. If the \
             picture still looks unchanged, raise NR Intensity / Local Structure in its panel — \
             the default is subtle.",
        ));
    } else if rs.contains("feature=1 (DLSS/DLAA)") {
        out.push(warn(
            "The add-on saw the game's DLSS but has not evaluated the model yet (feature 18 never \
             created). Enable DLSS in the game's own graphics settings and enable neural rendering \
             in the add-on panel.",
        ));
    } else if st.mode == game::Mode::Native {
        if st.feeder {
            out.push(warn(
                "Mode is Native DLSS (game ships its own DLSS), but dlss5-feed.addon64 is still \
                 present. Feeder Optimize does not apply here — NR is game/renodx. Run Remove \
                 (incl. Feeder leftovers) or Install again so Native cleanup drops the Feeder.",
            ));
        }
        // The add-on hooks NVSDK_NGX_D3D12_*. A game whose DLSS runs on D3D11
        // calls the D3D11 entry points, which it never sees, so "no create"
        // is expected until the bridge is installed (#33, BG3 DX11).
        if st.api == game::Api::Dx11 && !st.bridge {
            out.push(bad(
                "No NGX call was intercepted, and this is a Direct3D 11 game with its own \
                 DLSS: the add-on hooks the D3D12 NGX entry points, but the game calls the \
                 D3D11 ones, so it can never see them. The DX11 bridge covers exactly this \
                 and is not installed here — run Install on this exe.",
            ));
        } else {
            out.push(bad(
                "No NGX call was intercepted: this game's own DLSS never ran. Turn DLSS on \
                 in the game's graphics settings (the add-on hooks the game's DLSS calls; \
                 without them it has nothing to work with).",
            ));
        }
    }

    // Linux/Proton: ReShade generates HLSL with [fastopt] for shader model 4
    // and up, and Wine's d3dcompiler_47 (vkd3d-shader) has not implemented it,
    // so DLSS5_Feed.fx and the Lumenite shaders never build — with the feed
    // add-on then reporting its technique missing, which reads like our bug
    // rather than a missing compiler (#70).
    if wine_hlsl {
        let line = rs
            .lines()
            .find(|l| l.contains("not yet implemented feature"))
            .unwrap_or("")
            .trim();
        out.push(bad(format!(
            "The effects failed to compile in Wine/Proton's own HLSL compiler: {line} \
             That message comes from vkd3d-shader, which Wine's d3dcompiler_47.dll uses; \
             ReShade emits attributes it has not implemented. What a reporter measured working \
             (#76, Proton 10.0-4, Elden Ring, no launch options at all): put a copy of \
             Microsoft's real d3dcompiler_47.dll in the game folder, beside the executable. \
             The feed then reports it as \"not System32, but it accepts cs_5_1 -- fine\" and \
             the effects compile. If the prefix still loads Wine's copy instead, add \
             WINEDLLOVERRIDES=\"d3dcompiler_47=n\" %command% to the launch options; \
             protontricks <appid> d3dcompiler_47 (or winetricks d3dcompiler_47) puts the real \
             one in the prefix if you do not have a copy to hand."
        )));
    }

    // The compile failure itself, which is unambiguous when it appears.
    if let Some(line) = rs
        .lines()
        .find(|l| l.contains("X3506") || l.contains("unrecognized compiler target"))
    {
        out.push(bad(format!(
            "{} — the HLSL compiler in this process is too old for the DLSS 5 pass. That is \
             a d3dcompiler_47.dll shipped with the game, loaded in preference to System32's. \
             Rename it (d3dcompiler_47.dll.bak) and start the game again.",
            line.trim()
        )));
    }

    // A game with more than one executable (a Vulkan build and a DX11 build,
    // a launcher and the game) can be installed for one and played through
    // another: ReShade loads, everything looks right, nothing is hooked (#33).
    if let Some(loaded) = reshade_host_exe(&rs).filter(|_| !st.is32()) {
        let ours = st
            .exe
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase());
        if ours.is_some_and(|o| o != loaded.to_ascii_lowercase()) {
            out.push(warn(format!(
                "ReShade loaded into {loaded}, but this install was set up for {}. Those are \
                 different executables, and the install is tuned to the one you picked (the \
                 DX11 bridge in particular). Point the tool at {loaded} and run Install again.",
                st.exe.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
    }

    // ── DX11 bridge (native DLSS on D3D11) ──────────────────────────
    if let Some(bl) = read(d, "dlss5-bridge.log") {
        if let Some(line) = bl.lines().rev().find(|l| l.contains("stopped:")) {
            out.push(bad(format!("The DX11 bridge stopped: {}", line.trim())));
        }
        if let Some(line) = bl
            .lines()
            .find(|l| l.contains("D3D12CreateDevice failed 0x887E0003"))
        {
            // Where the redist actually is decides what the user can do. Unreal
            // puts it in a D3D12 subfolder; a Unity player declares the exe's own
            // folder, so renaming a "D3D12 folder" that was never there changes
            // nothing and reads as a dead end (dlss5-bridge#24).
            let where_ = match game::has_agility_redist(d) {
                Some(p) => {
                    let ver = crate::ngx::file_version(&p).unwrap_or_else(|| "unknown".into());
                    format!(
                        "The copy in force here is {} ({ver}). Rename it and start the game \
                         again: it falls back to the Windows runtime, which every device in \
                         the process can match. If the game will not start without it, verify \
                         the game files instead -- a D3D12Core.dll replaced or truncated by \
                         another tool gives exactly this error.",
                        p.display()
                    )
                }
                None => "No D3D12Core.dll is next to the exe or in a D3D12 folder here, so the \
                         declaration points somewhere else or the file is missing outright. \
                         Verify the game files."
                    .into(),
            };
            out.push(bad(format!(
                "{} — 0x887E0003 is D3D12_ERROR_INVALID_REDIST: the executable declares its own \
                 DirectX 12 Agility SDK (D3D12SDKVersion/D3D12SDKPath exports), and until that \
                 declaration is satisfied no D3D12 device can be created in this process at \
                 all -- not the bridge's, not the game's. Not something this tool sets. {}",
                line.trim(),
                where_
            )));
        } else if bl.contains("frames:") && !bl.contains("session failed") {
            out.push(ok(
                "The DX11 bridge opened its D3D12 session and is delivering frames.",
            ));
        }
    }

    // ── Feeder side (games without DLSS) ────────────────────────────
    if st.mode == game::Mode::Feeder {
        let Some(fd) = read(d, "dlss5-feed.log") else {
            out.push(bad(
                "No dlss5-feed.log: DLSS5-Feeder never started. Its add-on is missing or disabled \
                 in ReShade's Add-ons tab.",
            ));
            return out;
        };
        if fd.contains("feature ready") {
            out.push(ok(
                "DLSS5-Feeder built its DLSS feature (feature ready … DLAA).",
            ));
        }
        if fd.contains("frame") && fd.contains("delivered") {
            out.push(ok("Frames were delivered to the model."));
        }
        if fd.contains("technique MISSING") && !fd.contains("technique found") {
            out.push(bad(
                "DLSS5_Feed.fx is not compiling. Its shader files are missing from \
                 reshade-shaders\\Shaders — re-run Install.",
            ));
        }
        // The first effects line of a session always says "none": effects are
        // not compiled yet. Only the last one describes the running state (#6).
        let last_effects = fd
            .lines()
            .rev()
            .find(|l| l.contains("[feed] effects:"))
            .unwrap_or("");
        if last_effects.contains("-> none (not installed)") {
            out.push(bad(
                "The motion-vector provider is not enabled. In ReShade's Home tab enable \
                 \"LUMENITE: Kernel 2.0\" ABOVE \"DLSS5_Feed\", then reload effects.",
            ));
        }
        ngx_init_failure_for(&fd, Some(&st.exe), &mut out);
        // 32-bit games: the work happens in host64\, and its own log names the reason.
        if let Some(hl) = read(&d.join(game::HOST_DIR), "dlss5-feed-host.log") {
            if hl.contains("feature ready") {
                out.push(ok(
                    "The host64 helper built its DLSS feature (feature ready … DLAA).",
                ));
            }
            ngx_init_failure_for(&hl, Some(&st.consumer_dir().join(game::HOST_EXE)), &mut out);
        } else if st.is32() {
            out.push(warn(
                "No host64\\dlss5-feed-host.log yet: the 64-bit helper has not started. It is \
                 spawned by the first fed frame, so enable Lumenite_Kernel + DLSS5_Feed in \
                 ReShade's Home tab and play a moment first.",
            ));
        }
        if let Some(ver) = fd
            .lines()
            .next()
            .and_then(|l| {
                // "HH:MM:SS.mmm  dlss5-feed 0.12.0 (built ...) attached."
                let mut it = l.split_whitespace();
                it.find(|t| t.starts_with("dlss5-feed"))?;
                it.next()
            })
            .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            if version_key(ver) < version_key(CURRENT_FEEDER) {
                out.push(warn(format!(
                    "DLSS5-Feeder {ver} in the log is older than {CURRENT_FEEDER}; re-run Install \
                     to refresh it (since 0.9.1 an existing Feeder is updated)."
                )));
            }
        }
        if fd.contains("MV probe") && fd.contains("0% non-zero") {
            out.push(bad(
                "Motion vectors are all zero: the provider is enabled but writes nothing. Check \
                 that Lumenite_Kernel sits above DLSS5_Feed in the technique list.",
            ));
        }
        if fd.contains("DLSS super sampling is not available") {
            out.push(bad(
                "NGX reported DLSS unavailable. nvngx_dlss.dll must sit next to the game exe — \
                 re-run Install, and make sure antivirus did not remove it.",
            ));
        }
        // Some games refuse the reduced work-resolution path: the feed builds
        // its shared textures, the staging SRV for the smaller image fails, and
        // three failed builds stop the feed. Nothing downstream then happens —
        // the add-on's overlay says "HOOKS ARMED - NO DLSS CREATE SEEN" and
        // toggling neural rendering in game does nothing, which reads like the
        // add-on is broken rather than one setting being wrong (#74).
        // The 32-bit halves talk a versioned protocol. When they disagree the
        // host exits immediately and the game shows nothing at all (#69).
        if let Some(line) = fd
            .lines()
            .chain(
                read(&st.consumer_dir(), "dlss5-feed-host.log")
                    .as_deref()
                    .unwrap_or("")
                    .lines(),
            )
            .find(|l| l.contains("speaks protocol v") && l.contains("this host v"))
        {
            out.push(bad(format!(
                "The two halves of the 32-bit install are from different releases: {} \
                 Run Install again — since 0.13.4 both halves are taken from one download and \
                 each records the release it came from, so this cannot happen silently.",
                line.trim()
            )));
        }
        // The host warns when the add-on's version and the driver are a pair it
        // has measured failing. Every build of that add-on carries the same
        // embedded FileVersion (0.2026.0828.0517 in 4.55 and 4.70 alike), so
        // the warning fires on the classic build too — the one the host's own
        // text calls a way through. Only the tag this tool recorded can say
        // which build is actually on disk (#69, and upstream DLSS5-Feeder#90).
        if fd.contains("is a combination measured to fail")
            || read(&st.consumer_dir(), "dlss5-feed-host.log")
                .as_deref()
                .is_some_and(|l| l.contains("is a combination measured to fail"))
        {
            let tag = read(&st.consumer_dir(), crate::game::DLSS5_ADDON_MARKER)
                .map(|t| t.trim().to_owned())
                .unwrap_or_default();
            if tag == crate::installer::RENODX_CLASSIC_TAG {
                out.push(ok(format!(
                    "The host warns that this add-on build and your driver are a combination it \
                     measured failing. You are already on the classic build it recommends \
                     ({tag}) — every build of that add-on reports the same FileVersion, so the \
                     host cannot tell them apart and warns either way. Nothing to do here."
                )));
            } else {
                out.push(warn(format!(
                    "The host measured this add-on build failing on your driver. Run Install \
                     again: it reads that verdict out of this log and pins the classic build \
                     ({}) by itself.",
                    crate::installer::RENODX_CLASSIC_TAG
                )));
            }
        }
        // The create faults inside the driver rather than returning a code. The
        // feed catches it, cannot retry (the consumer's own locks were skipped
        // by the unwind), and stops — so the game runs and nothing happens,
        // with the reason six lines up in a log nobody reads (#76).
        if fd.contains("CreateFeature raised 0xC0000005") {
            let stack = fd
                .lines()
                .find(|l| l.contains("CreateFeature fault stack"))
                .map(|l| l.split("(innermost first):").nth(1).unwrap_or(l).trim())
                .unwrap_or("")
                .to_owned();
            let two_modules = fd.contains("two copies of the DLSS NGX module are loaded");
            let mut t = "The DLSS feature create faulted inside the driver (access violation),                  and the feed stopped: it cannot safely call back in, because the neural                  consumer's own code was on the faulting stack and its locks were skipped by the                  unwind."
                .to_owned();
            if !stack.is_empty() {
                t.push_str(&format!(" Fault stack: {stack}."));
            }
            if two_modules {
                // Tested on Proton and it is the wrong advice there: with the
                // game-local DLL moved aside, the driver's own NGX answers
                // 0xBAD00012 (NotImplemented) for SuperSampling and DLSS is not
                // available at all, which is worse than the crash (#76).
                if wine_hlsl || fd.contains("driver 999.99") {
                    t.push_str(
                        " On Windows the usual next step is moving the game-local nvngx_dlss.dll \
                         aside, because two copies of the NGX module are loaded and the add-on \
                         hooks both. Do NOT do that here: this is Wine/Proton, where the driver's \
                         own NGX does not provide DLSS, and removing the game-local copy has been \
                         measured to leave DLSS unavailable entirely.",
                    );
                } else {
                    t.push_str(
                        " The log names the most likely cause: two copies of the DLSS NGX module \
                         are loaded — the game-local nvngx_dlss.dll and the driver's own \
                         _nvngx.dll — and the add-on hooks both. Try moving nvngx_dlss.dll out of \
                         the game folder (to nvngx_dlss.dll.off) and starting the game again \
                         WITHOUT re-running Install, which would put it back.",
                    );
                }
            }
            out.push(bad(t));
        }
        if fd.contains("work-resolution staging SRV failed") {
            let pct = fd
                .lines()
                .find(|l| l.contains("work resolution ("))
                .and_then(|l| l.split("work resolution (").nth(1))
                .and_then(|r| r.split(')').next())
                .unwrap_or("below 100%")
                .to_owned();
            out.push(bad(format!(
                "The feed could not build its textures at {pct} of the frame: \
                 \"work-resolution staging SRV failed\", three times, and then it stopped. This \
                 game does not accept the reduced work-resolution path. Set it back to full size \
                 — Settings ▸ Feeder knobs ▸ work_resolution = 100 and work_upscale = 0, or pick \
                 the High quality preset — and start the game again. Everything downstream of this \
                 (no DLSS create, the in-game toggle doing nothing) follows from it."
            )));
        }
        if fd.contains("stopped:") {
            let line = fd
                .lines()
                .rev()
                .find(|l| l.contains("stopped:"))
                .unwrap_or("")
                .trim()
                .to_string();
            out.push(bad(format!("The feed stopped itself: {line}")));
        }
        if fd.contains("CRASH RECORDED") {
            out.push(warn(
                "The feed recorded a crash inside the DLSS 5 add-on (upstream issue #16). Play in \
                 borderless/windowed rather than exclusive fullscreen, and raise create_delay in \
                 dlss5-feed.cfg.",
            ));
        }
    }
    out
}

/// Read the game folder and produce findings, or a single fatal one.
pub fn run(exe: &Path) -> Result<Vec<Finding>> {
    let st = game::inspect(exe)?;
    Ok(diagnose(&st))
}

/// What matiasLombo's neural-upstream (`nvngx.dll.addon64`, "DLSS5 NR
/// Pre-Upscale") wrote to ReShade.log, read against its own source: the game's
/// DLSS is logged as `CreateFeature id=1` (a DLSS create carries no DLSSNR
/// slots, so `<no slot>` there is normal), every DLSS evaluate as `eval feat=`,
/// and the add-on's own network as `2b: SNIPPET CreateFeature(18, …)` followed
/// by `2c: NR Evaluate(…) -> 0x00000001` once it ran.
fn upstream_findings(rs: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(l) = rs
        .lines()
        .find(|l| l.contains("Failed to load add-on") && l.contains("nvngx.dll.addon64"))
    {
        let code = l
            .rsplit("error code ")
            .next()
            .unwrap_or("")
            .trim_end_matches('!');
        out.push(bad(format!(
            "ReShade refused to load nvngx.dll.addon64 (Neural Upstream), error code {code}."
        )));
        return out;
    }
    if rs.contains("[NRPRE] addon registered") || rs.contains("DLSS5 NR Pre-Upscale") {
        out.push(ok(
            "The Neural Upstream add-on (DLSS5 NR Pre-Upscale) registered.",
        ));
    } else {
        out.push(bad(
            "The Neural Upstream add-on never registered. nvngx.dll.addon64 is missing from              the game folder, disabled in ReShade's Add-ons tab, or quarantined by antivirus.",
        ));
        return out;
    }
    if rs.contains("[NRPRE] no NGX evaluate could be hooked") {
        out.push(bad(
            "Neural Upstream could not hook the game's NGX evaluate: the game's DLSS runtime              was not loaded when the add-on looked for it. Turn DLSS on in the game's graphics              settings and restart the game.",
        ));
        return out;
    }
    let nr_ran = rs
        .lines()
        .any(|l| l.contains("2c: NR Evaluate(") && l.contains("-> 0x00000001"));
    let nr_failed = rs
        .lines()
        .find(|l| l.contains("NR Evaluate(") && l.contains("FAILED"));
    let snippet_failed = rs
        .lines()
        .find(|l| l.contains("SNIPPET CreateFeature(18") && !l.contains("-> 0x00000001"));
    if nr_ran {
        out.push(ok(
            "Neural rendering ran: Neural Upstream evaluated the DLSS 5 model at render              resolution on real frames. If the picture looks unchanged, raise EffectStrength              (F6) in the add-on's ReShade panel.",
        ));
    } else if let Some(l) = nr_failed {
        out.push(bad(format!(
            "Neural Upstream created its network but evaluating it fails: {}",
            l.trim()
        )));
    } else if let Some(l) = snippet_failed {
        out.push(bad(format!(
            "Neural Upstream could not create the DLSS 5 network: {}",
            l.trim()
        )));
    } else if rs.contains("2b: core exports missing") {
        out.push(bad(
            "nvngx_dlssnr.dll beside the game is not the build Neural Upstream expects              (its NGX exports are missing). Run Install again to refresh the model.",
        ));
    } else if rs.contains("[NRPRE] eval feat=") {
        out.push(warn(
            "The game's DLSS is running frames through the add-on, but the DLSS 5 network was              never created. Press F7 in game (DLSS-NR on/off) and check the add-on's panel;              if it stays off, attach this log.",
        ));
    } else if rs.contains("[NRPRE] CreateFeature id=") {
        out.push(warn(
            "The game created its DLSS feature but never rendered a frame through it: the              session ended at a menu or loading screen. Load into gameplay, play for a minute,              then run this again.",
        ));
    } else {
        out.push(bad(
            "No NGX call was intercepted: this game's own DLSS never ran. Turn DLSS on in the              game's graphics settings (the add-on hooks the game's DLSS calls; without them it              has nothing to work with).",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    /// Baldur's Gate 3 ships bg3.exe (Vulkan) and bg3_dx11.exe; installing for
    /// one and playing the other leaves everything looking right and nothing
    /// hooked (#33). The exe name is in ReShade's first line.
    #[test]
    fn reshade_host_exe_reads_the_first_line() {
        let log = "01:30:11:810 [17792] | INFO  | Initializing crosire's ReShade version '6.8.0.2155' (64-bit) loaded from 'C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\Baldurs Gate 3\\\\bin\\\\dxgi.dll' into 'C:\\\\Program Files (x86)\\\\Steam\\\\steamapps\\\\common\\\\Baldurs Gate 3\\\\bin\\\\bg3_dx11.exe' (0x64317982) ...";
        assert_eq!(
            super::reshade_host_exe(log).as_deref(),
            Some("bg3_dx11.exe")
        );
        assert!(super::reshade_host_exe("nothing useful here").is_none());
    }

    use super::*;
    use crate::game::testutil::*;

    fn setup(feeder: bool) -> (tempfile::TempDir, std::path::PathBuf) {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        if !feeder {
            fs::write(t.path().join(game::DLSS_DLL), b"x").unwrap();
        }
        (t, exe)
    }

    /// A 32-bit game runs the add-on under the 64-bit ReShade in host64\, so
    /// that is the log to read. Reading the game folder's log — the feeder's
    /// own 32-bit ReShade, which never loads the add-on — reported "the add-on
    /// never registered" on installs that were fine (#69).
    #[test]
    fn thirty_two_bit_reads_the_host64_reshade_log() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let host = t.path().join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        // The 32-bit ReShade beside the exe: no add-on, and never will have one.
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n",
        )
        .unwrap();
        // The one that matters, in host64\.
        fs::write(
            host.join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Ok && x.text.contains("add-on registered")),
            "{f:?}"
        );
        assert!(
            !f.iter().any(|x| x.text.contains("never registered")),
            "{f:?}"
        );
    }

    /// And when host64\ has no ReShade log at all, say so in host64 terms
    /// rather than claiming ReShade never loaded beside the exe.
    #[test]
    fn thirty_two_bit_missing_host64_log_names_host64() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().any(|x| x.text.contains("host64")), "{f:?}");
    }

    /// Under Proton the effects are compiled by Wine's d3dcompiler_47
    /// (vkd3d-shader), which has not implemented [fastopt]. Saying "the add-on
    /// never registered" or "rename your d3dcompiler" sends the user in the
    /// wrong direction — the second one actively removes the working compiler (#70).
    #[test]
    fn wine_hlsl_compiler_is_named_and_rename_advice_suppressed() {
        let (t, exe) = setup(true);
        fs::write(t.path().join("d3dcompiler_47.dll"), b"MZ").unwrap();
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n\
             ERROR | Failed to compile 'DLSS5_Feed.fx':\n\
             <anonymous>:118:13: E5017: Aborting due to not yet implemented feature: Unhandled attribute 'fastopt'.\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Bad && x.text.contains("vkd3d-shader")),
            "{f:?}"
        );
        assert!(
            !f.iter().any(|x| x.text.contains("d3dcompiler_47.dll.bak")),
            "{f:?}"
        );
        // The recipe a reporter measured working, not just "install a compiler" (#76).
        assert!(
            f.iter().any(|x| x.text.contains("WINEDLLOVERRIDES")),
            "{f:?}"
        );
    }

    /// Dying Light refuses the reduced work-resolution path. The user sees a
    /// stopped feed and an add-on saying it never saw a DLSS create, with
    /// nothing naming the one setting responsible (#74).
    #[test]
    fn work_resolution_build_failure_names_the_setting() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] building: 2176x1224 work resolution (85%) -> 2560x1440 backbuffer\n\
             [feed] work-resolution staging SRV failed\n\
             [feed] failure: resource build\n\
             stopped: repeated failures. The game renders normally.\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        let hit = f
            .iter()
            .find(|x| x.text.contains("work-resolution staging SRV failed"))
            .unwrap_or_else(|| panic!("{f:?}"));
        assert_eq!(hit.level, Level::Bad);
        assert!(hit.text.contains("85%"), "{}", hit.text);
        assert!(hit.text.contains("work_resolution = 100"), "{}", hit.text);
    }

    /// "Home does nothing" on a 32-bit game means the ReShade beside the exe is
    /// missing, which is upstream of every host64 finding. Diagnose used to
    /// report only the host64 side and leave the player looking in the wrong
    /// folder (#69).
    #[test]
    fn thirty_two_bit_missing_game_side_reshade_is_named_first() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let host = t.path().join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        fs::write(
            host.join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Bad && x.text.contains("No ReShade beside the game exe")),
            "{f:?}"
        );
    }

    /// A create that faults inside the driver stops the feed for good, and the
    /// log's own hint about two NGX modules is the actionable part. Neither
    /// reached the user (#76).
    #[test]
    fn a_faulting_feature_create_is_explained_with_its_stack() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] CreateFeature raised 0xC0000005 (reading address 00000000575284C0) (caught; nothing submitted)\n\
             [feed] CreateFeature fault stack, by module (innermost first): d3d12core.dll <- dxgi.dll <- nvapi64.dll <- nvngx_dlss.dll <- renodx-dlss5.addon64\n\
             [feed] two copies of the DLSS NGX module are loaded (the game-local nvngx_dlss.dll and the driver's _nvngx.dll)\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        let hit = f
            .iter()
            .find(|x| x.text.contains("faulted inside the driver"))
            .unwrap_or_else(|| panic!("{f:?}"));
        assert_eq!(hit.level, Level::Bad);
        assert!(hit.text.contains("d3d12core.dll"), "{}", hit.text);
        assert!(hit.text.contains("nvngx_dlss.dll.off"), "{}", hit.text);
    }

    /// GTA IV reads the adapter's VRAM and sizes its pools from it, so dgVoodoo
    /// reporting 4096 MB without a matching -availablevidmem is a black screen
    /// after load, and no conf at all is TEXP60 at startup (#69).
    #[test]
    fn dgvoodoo_vram_without_availablevidmem_is_flagged() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\n",
        )
        .unwrap();
        fs::write(t.path().join("dgVoodoo.conf"), "[DirectX]\nVRAM = 4096\n").unwrap();
        fs::write(t.path().join("commandline.txt"), "-norestrictions\n").unwrap();
        let f = run(&exe).unwrap();
        let hit = f
            .iter()
            .find(|x| x.text.contains("-availablevidmem"))
            .unwrap_or_else(|| panic!("{f:?}"));
        assert_eq!(hit.level, Level::Warn);
        assert!(hit.text.contains("4096"), "{}", hit.text);

        // Already set: nothing to say.
        fs::write(
            t.path().join("commandline.txt"),
            "-availablevidmem 4032\n-norestrictions\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            !f.iter().any(|x| x.text.contains("-availablevidmem")),
            "{f:?}"
        );
    }

    /// Moving the game-local nvngx_dlss.dll aside is the right advice on
    /// Windows and the wrong advice under Proton, where the driver's own NGX
    /// answers NotImplemented and DLSS disappears entirely. 0batsy tested both
    /// and neither helped, but the second left him worse off (#76).
    #[test]
    fn the_nvngx_advice_is_withheld_under_proton() {
        let (t, exe) = setup(true);
        let crash = "[feed] CreateFeature raised 0xC0000005 (caught; nothing submitted)\n\
                     [feed] two copies of the DLSS NGX module are loaded (the game-local nvngx_dlss.dll and the driver's _nvngx.dll)\n";
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();

        // Windows: the advice stands.
        fs::write(t.path().join("dlss5-feed.log"), crash).unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter().any(|x| x.text.contains("nvngx_dlss.dll.off")),
            "{f:?}"
        );

        // Proton, identified by the adapter line vkd3d reports.
        fs::write(
            t.path().join("dlss5-feed.log"),
            format!("[feed] adapter: NVIDIA GeForce RTX 4070 SUPER driver 999.99\n{crash}"),
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            !f.iter().any(|x| x.text.contains("nvngx_dlss.dll.off")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|x| x.text.contains("Do NOT do that here")),
            "{f:?}"
        );
    }

    /// The host's warning keys on a FileVersion every build of that add-on
    /// shares, so it fires on the classic build the host itself recommends.
    /// Only the tag this tool recorded can tell them apart (#69).
    #[test]
    fn the_measured_to_fail_warning_reads_the_recorded_tag() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] WARNING: renodx-dlss5 v4.6 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();

        // No tag on disk: the advice is to re-run Install, which pins it.
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Warn && x.text.contains("pins the classic build")),
            "{f:?}"
        );

        // Already pinned: say so instead of sending them round again.
        fs::write(
            t.path().join(crate::game::DLSS5_ADDON_MARKER),
            crate::installer::RENODX_CLASSIC_TAG,
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(
            f.iter()
                .any(|x| x.level == Level::Ok && x.text.contains("cannot tell them apart")),
            "{f:?}"
        );
    }

    #[test]
    fn no_reshade_log_is_fatal() {
        let (_t, exe) = setup(true);
        let f = run(&exe).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, Level::Bad);
        assert!(f[0].text.contains("never loaded"));
    }

    #[test]
    fn native_without_game_dlss_call() {
        let (t, exe) = setup(false);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade version '6.8.0'\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f
            .iter()
            .any(|x| x.text.contains("game's own DLSS never ran")));
        assert!(f
            .iter()
            .any(|x| x.level == Level::Ok && x.text.contains("add-on registered")));
    }

    #[test]
    fn addon_load_failure_is_explained() {
        let (t, exe) = setup(false);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nFailed to load add-on from 'C:\\g\\renodx-dlss5.addon64' with error code 2148073478!\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().any(|x| x.text.contains("unsigned DLLs")));
    }

    #[test]
    fn feeder_provider_not_enabled() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] effects: DLSS5_Feed.fx technique found, DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> none (not installed)\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f
            .iter()
            .any(|x| x.text.contains("motion-vector provider is not enabled")));
    }

    #[test]
    fn feeder_last_effects_line_wins_and_ngx_init_failure_named() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "12:33:33.151  dlss5-feed 0.7.0 (built Aug 31 2026) attached.\n\
             [feed] effects: DLSS5_Feed.fx technique MISSING, DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> none (not installed)\n\
             [feed] effects: DLSS5_Feed.fx technique found, DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> Lumenite_Kernel (enabled)\n\
             [feed] NVSDK_NGX_D3D12_Init -> 0xBAD00001 (FeatureNotSupported)\n\
             stopped: the D3D12/NGX session failed to start.\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(!f
            .iter()
            .any(|x| x.text.contains("motion-vector provider is not enabled")));
        assert!(f
            .iter()
            .any(|x| x.text.contains("NGX refused to initialise")));
        assert!(f
            .iter()
            .any(|x| x.text.contains("0.7.0 in the log is older")));
    }

    #[test]
    fn healthy_session_says_so() {
        let (t, exe) = setup(true);
        fs::write(
            t.path().join("ReShade.log"),
            "Initializing crosire's ReShade\nRegistered add-on \"DLSS 5 Neural Rendering\"\ninline feature 18 evaluation succeeded (count=60)\n",
        )
        .unwrap();
        fs::write(
            t.path().join("dlss5-feed.log"),
            "[feed] feature ready: 3840x2160 DLAA\n[feed] frame 1 delivered\n",
        )
        .unwrap();
        let f = run(&exe).unwrap();
        assert!(f.iter().all(|x| x.level == Level::Ok), "{f:?}");
        assert!(f.iter().any(|x| x.text.contains("raise NR Intensity")));
    }

    /// The lines felipeptxa's RDR2 log carried (#86): the add-on registered,
    /// hooked, and saw the game create its DLSS feature, then the session
    /// ended at the menu. Read as the RenoDX add-on's log this was two FAILs.
    #[test]
    fn neural_upstream_log_is_read_by_its_own_lines() {
        let menu = "Registered add-on \"DLSS5 NR Pre-Upscale\"
            [NRPRE] addon registered (NR at render resolution -- configure in the ReShade overlay)
            [NRPRE] hook 0 on NVSDK_NGX_D3D12_EvaluateFeature: OK
            [NRPRE] hook on NVSDK_NGX_D3D12_CreateFeature: OK (module 0)
            [NRPRE] CreateFeature id=1 -> res=0x00000001 handle=1 | NR W=4294967295 H=4294967295 ratio=-1.000 preset=-1 | DLSSNR.Color=<no slot> | DLSSNR.Output=<no slot> | DLSSNR.Depth=<no slot> | DLSSNR.MVec=<no slot>
            [NRPRE] F11: cadence phase -> 0/1
";
        let f = upstream_findings(menu);
        assert!(
            f.iter()
                .any(|x| x.level == Level::Ok && x.text.contains("registered")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|x| x.text.contains("never rendered a frame")),
            "{f:?}"
        );
        assert!(
            !f.iter().any(|x| x.text.contains("never registered")),
            "{f:?}"
        );
        assert!(!f.iter().any(|x| x.text.contains("No NGX call")), "{f:?}");

        let ran = format!(
            "{menu}[NRPRE] eval feat=1  render_subrect=1707x960 | a | b | c | d
             [NRPRE] 2b: SNIPPET CreateFeature(18, 1707x960) -> 0x00000001 handle=000001
             [NRPRE] 2c: NR Evaluate(HI) -> 0x00000001
"
        );
        let f = upstream_findings(&ran);
        assert!(
            f.iter()
                .any(|x| x.level == Level::Ok && x.text.contains("Neural rendering ran")),
            "{f:?}"
        );

        let failed = format!(
            "{menu}[NRPRE] eval feat=1  render_subrect=1707x960 | a | b | c | d
             [NRPRE] 2b: SNIPPET CreateFeature(18, 1707x960) -> 0xBAD00002 handle=0000000000000000
"
        );
        let f = upstream_findings(&failed);
        assert!(
            f.iter()
                .any(|x| x.level == Level::Bad && x.text.contains("0xBAD00002")),
            "{f:?}"
        );
    }
}
