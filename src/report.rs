//! One zip with everything a bug report needs: the logs and inis this tool and
//! its components write, Diagnose's findings, a listing of the game folder,
//! and what the machine is. Half the tracker was three replies of "please
//! attach X" before the file that mattered showed up.

use crate::{diagnose, game, gpu, settings};
use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Files worth carrying, relative to the game folder. Absent ones are skipped.
const FILES: [&str; 19] = [
    "ReShade.log",
    "ReShade.log1",
    "ReShade.log2",
    "ReShade.ini",
    "ReShadePreset.ini",
    "ReShadeVR.ini",
    "dlss5-feed.log",
    "dlss5-feed.cfg",
    "OptiScaler.log",
    "OptiScaler.ini",
    "re2_framework_log.txt",
    "nrpre-ring.txt",
    "dgVoodoo.conf",
    "host64/ReShade.log",
    "host64/ReShade.ini",
    "host64/dlss5-feed-host.log",
    "host64/ReShadePreset.ini",
    ".dlss5oneclick-optiscaler-manifest",
    ".dlss5oneclick-aio-manifest",
];

/// A log can run to hundreds of megabytes; the end is where the answer is.
const TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// Write the bundle beside the user's Desktop (or the temp folder when there is
/// none) and return its path.
pub fn write_bundle(exe: &Path) -> Result<PathBuf> {
    let d = exe.parent().context("exe has no parent")?;
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "game".into());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_secs())
        .unwrap_or(0);
    let out_dir = std::env::var_os("USERPROFILE")
        .map(|u| PathBuf::from(u).join("Desktop"))
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let out = out_dir.join(format!("dlss5oneclick-report-{stem}-{stamp}.zip"));

    let file = fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("summary.txt", opts)?;
    zip.write_all(summary(exe).as_bytes())?;

    zip.start_file("folder.txt", opts)?;
    zip.write_all(listing(d).as_bytes())?;

    for rel in FILES {
        let p = d.join(rel);
        if let Some(bytes) = tail(&p) {
            zip.start_file(format!("game/{rel}"), opts)?;
            zip.write_all(&bytes)?;
        }
    }
    // The sidecars: which release of each piece this tool placed.
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with(".dlss5oneclick") {
                if let Ok(t) = fs::read_to_string(e.path()) {
                    zip.start_file(format!("game/{n}"), opts)?;
                    zip.write_all(t.as_bytes())?;
                }
            }
        }
    }
    if let Some(bytes) = tail(&settings::Settings::path()) {
        zip.start_file("settings.json", opts)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(out)
}

fn summary(exe: &Path) -> String {
    let mut s = format!(
        "dlss5oneclick {}\nexe: {}\n",
        env!("CARGO_PKG_VERSION"),
        exe.display()
    );
    if let Some(v) = gpu::nvidia_driver() {
        s.push_str(&format!("nvidia driver: {v}\n"));
    }
    match game::inspect(exe) {
        Ok(st) => {
            s.push_str(&format!(
                "{}-bit | {} (detected {}) | mode={:?} (detected {:?}) | reshade={} headers={} feeder={} lumenite={} dlss5={} upstream={} aio={} dlssnr={} dlss={} bridge={} opti={} mfg={} reframework={} | gpu={} | complete={}\n",
                st.bitness,
                st.api.label(),
                st.api_detected.label(),
                st.mode,
                st.mode_detected,
                st.reshade,
                st.headers,
                st.feeder,
                st.lumenite,
                st.dlss5_addon,
                st.upstream,
                st.aio,
                st.dlssnr,
                st.dlss,
                st.bridge,
                st.opti,
                st.mfg,
                st.reframework,
                st.gpu
                    .as_ref()
                    .map(|(g, t)| format!("{} [{}]", g.name, t.label()))
                    .unwrap_or_else(|| "unknown".into()),
                st.complete()
            ));
            for p in &st.problems {
                s.push_str(&format!("! {p}\n"));
            }
            s.push_str("\n--diagnose:\n");
            for f in diagnose::diagnose(&st) {
                let tag = match f.level {
                    diagnose::Level::Ok => "ok  ",
                    diagnose::Level::Warn => "warn",
                    diagnose::Level::Bad => "FAIL",
                };
                s.push_str(&format!("[{tag}] {}\n", f.text));
            }
        }
        Err(e) => s.push_str(&format!("inspect failed: {e:#}\n")),
    }
    s
}

/// Name and size of everything in the game folder, plus `host64\` and the
/// shader folders, one level each. Enough to see which proxy DLLs are there.
fn listing(d: &Path) -> String {
    let mut s = String::new();
    for sub in [
        "",
        "host64",
        "reshade-shaders/Shaders",
        "reshade-shaders/Shaders/include",
        "reshade-shaders/Textures",
    ] {
        let p = if sub.is_empty() {
            d.to_path_buf()
        } else {
            d.join(sub)
        };
        let Ok(rd) = fs::read_dir(&p) else { continue };
        s.push_str(&format!("[{}]\n", if sub.is_empty() { "." } else { sub }));
        let mut rows: Vec<(String, u64, bool)> = rd
            .flatten()
            .map(|e| {
                let m = e.metadata().ok();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    m.as_ref().map(|m| m.len()).unwrap_or(0),
                    m.is_some_and(|m| m.is_dir()),
                )
            })
            .collect();
        rows.sort();
        for (n, len, dir) in rows {
            if dir {
                s.push_str(&format!("  {n}/\n"));
            } else {
                s.push_str(&format!("  {n}  {len}\n"));
            }
        }
    }
    s
}

/// The last `TAIL_BYTES` of a file, or all of it when smaller; `None` when it
/// does not exist or cannot be read.
fn tail(p: &Path) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let mut out = Vec::new();
    if len > TAIL_BYTES {
        f.seek(SeekFrom::Start(len - TAIL_BYTES)).ok()?;
        out.extend_from_slice(
            format!("[... first {} bytes omitted ...]\n", len - TAIL_BYTES).as_bytes(),
        );
    }
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::testutil::make_pe;

    #[test]
    fn bundle_carries_logs_markers_listing_and_summary() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        fs::create_dir_all(d.join("host64")).unwrap();
        fs::write(d.join("ReShade.log"), "Initializing crosire's ReShade").unwrap();
        fs::write(d.join("host64").join("ReShade.log"), "host").unwrap();
        fs::write(d.join("nvngx_dlss.dll.dlss5oneclick"), "v310.9.1").unwrap();
        // Desktop is whatever this machine has; the file lands there or in temp.
        let out = write_bundle(&exe).unwrap();
        let f = fs::File::open(&out).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let names: Vec<String> = z.file_names().map(str::to_owned).collect();
        for want in [
            "summary.txt",
            "folder.txt",
            "game/ReShade.log",
            "game/host64/ReShade.log",
            "game/nvngx_dlss.dll.dlss5oneclick",
        ] {
            assert!(
                names.iter().any(|n| n == want),
                "{want} missing from {names:?}"
            );
        }
        let mut s = String::new();
        std::io::Read::read_to_string(&mut z.by_name("summary.txt").unwrap(), &mut s).unwrap();
        assert!(s.contains("--diagnose:"), "{s}");
        assert!(s.contains("64-bit"), "{s}");
        fs::remove_file(out).unwrap();
    }

    #[test]
    fn tail_keeps_the_end_of_a_big_log() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("big.log");
        let mut body = vec![b'a'; (TAIL_BYTES + 10) as usize];
        body.extend_from_slice(b"THE END");
        fs::write(&p, &body).unwrap();
        let got = tail(&p).unwrap();
        assert!(got.starts_with(b"[... first 17 bytes omitted ...]"));
        assert!(got.ends_with(b"THE END"));
        assert!(tail(&t.path().join("none.log")).is_none());
    }
}
