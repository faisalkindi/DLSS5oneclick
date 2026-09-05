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
const CURRENT_FEEDER: &str = "0.12.0";

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

    // ── a game-shipped HLSL compiler shadowing the system one ──────
    // The add-on compiles its NR pass at cs_5_1. A d3dcompiler_47.dll that
    // ships with the game is loaded in preference to System32's, and an old
    // one does not know that target: "error X3506: unrecognized compiler
    // target" and no neural rendering, with everything else looking correct.
    let compiler = d.join("d3dcompiler_47.dll");
    if compiler.is_file() {
        let ver = crate::ngx::file_version(&compiler).unwrap_or_else(|| "unknown".into());
        out.push(warn(format!(
            "The game ships its own d3dcompiler_47.dll ({ver}), which Windows loads instead of \
             System32's. If it predates shader model 5.1 the DLSS 5 pass cannot compile \
             (error X3506). Rename it to d3dcompiler_47.dll.bak and start the game again; \
             almost every game runs fine on the system copy."
        )));
    }

    // ── which neural model is installed ────────────────────
    // Two builds of nvngx_dlssnr.dll are in circulation and only the version
    // resource separates them; every failing RTX 50 report so far carries the
    // .SF one, so the log has to name it.
    let consumer = st.consumer_dir();
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
    let Some(rs) = read(d, "ReShade.log").or_else(|| read(d, "ReShade2.log")) else {
        out.push(bad(
            "No ReShade.log next to the game exe: ReShade never loaded. Either the game was not \
             started since the install, or it does not load dxgi.dll (wrong exe picked, or a \
             launcher starts a different one). Check the exe with --check.",
        ));
        return out;
    };
    if rs.contains("Initializing crosire's ReShade") {
        out.push(ok("ReShade loaded into the game."));
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
        out.push(bad(
            "The DLSS 5 add-on never registered. renodx-dlss5.addon64 is missing from the game \
             folder, disabled in ReShade's Add-ons tab, or quarantined by antivirus.",
        ));
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
    if let Some(loaded) = reshade_host_exe(&rs) {
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
}
