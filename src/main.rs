#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod diagnose;
mod game;
mod gpu;
mod gpupref;
mod gui;
mod installer;
mod library;
mod logo;
mod net;
mod ngx;
mod renodx;
mod reshade_ini;
mod text;
mod theme;
mod update;

use std::io::Write;
use std::path::PathBuf;

/// `dlss5oneclick <GAME.exe | game folder> [--remove | --remove-all | --check | --diagnose | --engine=opti | --renodx | --ignore-anticheat | --mode=feeder|native] | --update` runs headless; no args opens the GUI.
/// Read by the NVIDIA and AMD drivers from this exe's export table to choose
/// the discrete GPU for the whole process. Exported by the linker flags in
/// build.rs; the values themselves are what the drivers read (#32).
#[cfg(windows)]
#[no_mangle]
#[used]
pub static NvOptimusEnablement: u32 = 1;

#[cfg(windows)]
#[no_mangle]
#[used]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

fn main() {
    install_panic_handler();
    update::cleanup_old();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--fetch") {
        attach_parent_console();
        let (Some(url), Some(dest)) = (args.get(1), args.get(2)) else {
            eprintln!("usage: --fetch <url> <file>");
            std::process::exit(2);
        };
        let code = match net::client().and_then(|c| {
            net::download(&c, url, std::path::Path::new(dest), "fetch", &|p, m| {
                print!("\r{p:3}% {m:<72}");
                let _ = std::io::stdout().flush();
            })
        }) {
            Ok(()) => {
                println!(
                    "
ok"
                );
                0
            }
            Err(e) => {
                eprintln!(
                    "
error: {e:#}"
                );
                1
            }
        };
        std::process::exit(code);
    }
    if args.iter().any(|a| a == "--list-games") {
        attach_parent_console();
        for g in library::scan() {
            let when = g
                .installed
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            println!(
                "{:<10} {:<12} {:<50} {} poster={:?}",
                when,
                g.store.label(),
                g.title.chars().take(50).collect::<String>(),
                g.dir.display(),
                g.poster
            );
        }
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--posters") {
        // Support/diagnostic: decode every poster the scan resolved and report failures.
        attach_parent_console();
        let client = net::client().expect("http client");
        let (mut ok, mut bad) = (0usize, 0usize);
        for g in library::scan() {
            match library::poster_rgba(&client, &g.poster) {
                Some(img) => {
                    ok += 1;
                    println!(
                        "ok   {}x{} {} [{:?}]",
                        img.width(),
                        img.height(),
                        g.title,
                        g.poster
                    );
                }
                None => {
                    bad += 1;
                    println!("FAIL {} [{:?}]", g.title, g.poster);
                }
            }
        }
        println!("{ok} decoded, {bad} failed");
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--update") {
        attach_parent_console();
        std::process::exit(cli_update());
    }
    if args.iter().any(|a| a == "--ignore-anticheat") {
        game::set_ignore_anticheat(true);
    }
    if let Some(m) = args.iter().find_map(|a| a.strip_prefix("--mode=")) {
        std::env::set_var(game::MODE_ENV, m);
        if game::mode_override().is_none() {
            eprintln!("error: --mode must be feeder or native");
            std::process::exit(1);
        }
    }
    if let Some(first) = args.first().filter(|a| !a.starts_with('-')) {
        attach_parent_console();
        let code = cli(
            PathBuf::from(first),
            args.iter().any(|a| a == "--remove"),
            args.iter().any(|a| a == "--remove-all"),
            args.iter().any(|a| a == "--check"),
            args.iter().any(|a| a == "--diagnose"),
            if args.iter().any(|a| a == "--engine=opti" || a == "--opti") {
                installer::Engine::Opti
            } else {
                installer::Engine::ReShade
            },
            args.iter().any(|a| a == "--renodx"),
        );
        std::process::exit(code);
    }
    if let Err(e) = gui::run() {
        // The release build has no console: say it in a box and leave a file (#23).
        let msg = format!("{e:#}");
        eprintln!("gui error: {msg}");
        let log = write_error_log(&msg);
        report_gui_error(&format!(
            "DLSS5oneclick could not open its window.

{msg}

Written to {}
Attach that file to a GitHub issue.",
            log.display()
        ));
        std::process::exit(2);
    }
}

fn cli_update() -> i32 {
    match update::check() {
        Ok(None) => {
            println!("DLSS5oneclick {} is the latest version.", update::CURRENT);
            0
        }
        Ok(Some(av)) => {
            println!(
                "{} -> {} available. Downloading...",
                update::CURRENT,
                av.version
            );
            let progress = |pct: u8, msg: &str| {
                print!("\r{pct:3}% {msg:<72}");
                let _ = std::io::stdout().flush();
            };
            match update::download_and_swap(&av, &progress) {
                Ok(exe) => {
                    println!("\nUpdated to {} at {}", av.version, exe.display());
                    0
                }
                Err(e) => {
                    eprintln!("\nerror: {e:#}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

fn cli(
    target: PathBuf,
    remove: bool,
    remove_all: bool,
    check: bool,
    diagnose_only: bool,
    engine: installer::Engine,
    with_renodx: bool,
) -> i32 {
    let (exe, candidates) = match game::resolve_target(&target) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 1;
        }
    };
    if candidates.len() > 1 {
        let others: Vec<String> = candidates[1..]
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        println!(
            "using {} (other candidates: {})",
            exe.display(),
            others.join(", ")
        );
    } else if !candidates.is_empty() {
        println!("using {}", exe.display());
    }
    if diagnose_only {
        return match diagnose::run(&exe) {
            Ok(findings) => {
                for f in &findings {
                    let tag = match f.level {
                        diagnose::Level::Ok => "ok  ",
                        diagnose::Level::Warn => "warn",
                        diagnose::Level::Bad => "FAIL",
                    };
                    println!("[{tag}] {}", text::tidy(&f.text));
                }
                if findings.iter().any(|f| f.level == diagnose::Level::Bad) {
                    1
                } else {
                    0
                }
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    if check {
        return match game::inspect(&exe) {
            Ok(st) => {
                println!(
                    "{} | {}-bit | {} | mode={:?} | reshade={} headers={} feeder={} lumenite={} dlss5={} dlssnr={} dlss={} bridge={} opti={} | gpu={} | complete={}",
                    exe.display(), st.bitness, st.api.label(), st.mode, st.reshade, st.headers, st.feeder,
                    st.lumenite, st.dlss5_addon, st.dlssnr, st.dlss,
                    st.bridge,
                    st.opti,
                    st.gpu.as_ref().map(|(g, t)| format!("{} [{}]", g.name, t.label())).unwrap_or_else(|| "unknown".into()),
                    st.complete()
                );
                for p in &st.problems {
                    println!("  ! {}", text::tidy(p));
                }
                let names: Vec<&str> = installer::plan_with(&st, engine, with_renodx)
                    .iter()
                    .map(|s| s.name)
                    .collect();
                println!("  plan: {}", names.join(" -> "));
                if st.re_engine {
                    println!(
                        "  RE Engine game: REFramework (dinput8.dll) {}",
                        if st.reframework {
                            "present"
                        } else {
                            "missing, will be installed"
                        }
                    );
                }
                if gpupref::hybrid() {
                    // Which GPU Windows starts the process on decides whether NGX
                    // exists in it at all (#25).
                    println!(
                        "  hybrid machine ({}) · Windows GPU preference for {}: {}",
                        gpupref::real_adapters().join(", "),
                        exe.file_name().unwrap_or_default().to_string_lossy(),
                        gpupref::get(&exe).unwrap_or_else(|| "not set".into())
                    );
                }
                if game::installed_by_tool(st.game_dir()) {
                    // Same comparison the Games page makes, so a stale install
                    // can be seen without opening the window.
                    let latest = net::client()
                        .map(|c| installer::Latest::fetch(&c))
                        .unwrap_or_default();
                    match installer::stale_components(st.game_dir(), &latest).as_slice() {
                        [] => println!("  installed by this tool · everything current"),
                        stale => {
                            println!("  installed by this tool · out of date:");
                            for s in stale {
                                println!("    {s}");
                            }
                        }
                    }
                }
                if let Some(m) = &st.renodx_mod {
                    println!("  RenoDX mod installed: {m}");
                }
                if !st.foreign_renodx.is_empty() {
                    println!(
                        "  RenoDX mod present (not installed by this tool): {}",
                        st.foreign_renodx.join(", ")
                    );
                }
                match net::client().and_then(|c| renodx::lookup(&c, &exe)) {
                    Ok(Some(m)) => println!(
                        "  RenoDX HDR mod available: {} -> {} ({}){}",
                        m.title,
                        m.file,
                        m.status_label(),
                        if m.note.is_empty() {
                            String::new()
                        } else {
                            format!(" | {}", m.note)
                        }
                    ),
                    Ok(None) => println!("  RenoDX HDR mod: none for this game"),
                    Err(e) => println!("  RenoDX lookup failed: {e:#}"),
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    if remove_all {
        return match installer::uninstall_all(&exe) {
            Ok((list, kept)) => {
                for f in list {
                    println!("removed {f}");
                }
                if let Some(k) = kept {
                    println!("{k}");
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    if remove {
        return match installer::uninstall(&exe) {
            Ok(list) => {
                for f in list {
                    println!("removed {f}");
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
    }
    let progress = |pct: u8, msg: &str| {
        print!("\r{pct:3}% {msg:<72}");
        let _ = std::io::stdout().flush();
    };
    let step = move |i: usize, n: usize, name: &str, state: installer::StepState, detail: &str| {
        use installer::StepState::*;
        match state {
            Start => println!("\n[{}/{n}] {name}", i + 1),
            Done => println!("\n      ok: {detail}"),
            Error => println!("\n      FAILED: {detail}"),
        }
    };
    match installer::run_all_with(&exe, engine, with_renodx, &progress, &step) {
        Ok(_) => {
            if engine == installer::Engine::Opti {
                println!(
                    "
Done. In game: Insert opens the OptiScaler overlay -> enable Neural Rendering (off by default)."
                );
            } else {
                println!("
Done. In game: Home opens ReShade -> Add-ons tab -> DLSS 5 Neural Rendering -> enable. (Home tab saying no effect files is normal on games with their own DLSS.)");
            }
            0
        }
        Err(e) => {
            eprintln!("\nerror: {e:#}");
            1
        }
    }
}

/// Where a failure that has no console to print to is recorded (#23, #32).
fn error_log_path() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dlss5oneclick")
        .join("gui-error.txt")
}

fn write_error_log(msg: &str) -> std::path::PathBuf {
    let log = error_log_path();
    if let Some(p) = log.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(
        &log,
        format!(
            "DLSS5oneclick {}
{msg}
",
            env!("CARGO_PKG_VERSION")
        ),
    );
    log
}

/// A release build has `windows_subsystem = "windows"`, so a panic writes to a
/// stderr nobody can see and the process simply vanishes -- which is exactly
/// what two reporters described as "nothing happens" (#23, #32). Record it and
/// say so in a box instead.
fn install_panic_handler() {
    std::panic::set_hook(Box::new(|info| {
        let where_ = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".into());
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        let msg = format!("panic at {where_}: {what}");
        eprintln!("{msg}");
        let log = write_error_log(&msg);
        report_gui_error(&format!(
            "DLSS5oneclick stopped unexpectedly.

{msg}

Written to {}
Attach that file to a GitHub issue.",
            log.display()
        ));
    }));
}

#[cfg(windows)]
fn report_gui_error(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (t, c) = (wide(text), wide("DLSS5oneclick"));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            t.as_ptr(),
            c.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}
#[cfg(not(windows))]
fn report_gui_error(_text: &str) {}

/// Release builds hide the console; when launched from a terminal, reattach so CLI output shows.
fn attach_parent_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(test)]
mod tests {
    /// The crash log has to name the version, or a report cannot be matched to
    /// a build (#23, #32).
    #[test]
    fn error_log_records_version_and_message() {
        let p = super::write_error_log("panic at src/gui.rs:12: test");
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.starts_with(&format!("DLSS5oneclick {}", env!("CARGO_PKG_VERSION"))));
        assert!(body.contains("panic at src/gui.rs:12: test"));
        let _ = std::fs::remove_file(&p);
    }
}
