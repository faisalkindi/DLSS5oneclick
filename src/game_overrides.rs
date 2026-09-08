//! Local curated per-game Feeder cfg quirks (`assets/game_overrides.json`).
//! Matched by exe stem or path substring at Install — no cloud.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

const EMBEDDED: &str = include_str!("../assets/game_overrides.json");

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GameOverride {
    #[serde(default)]
    pub match_exe: Vec<String>,
    #[serde(default)]
    pub match_path: Vec<String>,
    #[serde(default)]
    pub note: String,
    pub ofa_enabled: Option<bool>,
    pub ofa_grid: Option<i32>,
    pub ofa_perf: Option<i32>,
    pub engine_velocity: Option<bool>,
    pub reset_mode: Option<i32>,
    pub light_stab: Option<bool>,
    pub light_stab_strength: Option<f32>,
    pub light_stab_max_delta: Option<f32>,
    pub work_resolution: Option<i32>,
    pub auto_profile_applied: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct FileRoot {
    #[serde(default)]
    overrides: Vec<GameOverride>,
}

fn load_overrides() -> Vec<GameOverride> {
    serde_json::from_str::<FileRoot>(EMBEDDED)
        .map(|r| r.overrides)
        .unwrap_or_default()
}

fn stem_matches(exe: &Path, patterns: &[String]) -> bool {
    let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let stem_l = stem.to_ascii_lowercase();
    patterns.iter().any(|p| {
        let p = p.to_ascii_lowercase();
        stem_l == p || stem_l.contains(&p) || p.contains(&stem_l)
    })
}

fn path_matches(exe: &Path, patterns: &[String]) -> bool {
    let path = exe.to_string_lossy().to_ascii_lowercase();
    patterns
        .iter()
        .any(|p| path.contains(&p.to_ascii_lowercase()))
}

/// First matching override for this game exe, if any.
pub fn find_override(exe: &Path) -> Option<GameOverride> {
    load_overrides().into_iter().find(|o| {
        (!o.match_exe.is_empty() && stem_matches(exe, &o.match_exe))
            || (!o.match_path.is_empty() && path_matches(exe, &o.match_path))
    })
}

/// Patch an existing `dlss5-feed.cfg` text with override keys (line replace / append).
pub fn apply_to_cfg_text(cfg: &str, o: &GameOverride) -> String {
    let mut lines: Vec<String> = cfg.lines().map(|l| l.to_string()).collect();

    let mut set = |key: &str, value: String| {
        let prefix = format!("{key}=");
        if let Some(i) = lines.iter().position(|l| {
            l.starts_with(&prefix)
                || l.to_ascii_lowercase()
                    .starts_with(&prefix.to_ascii_lowercase())
        }) {
            lines[i] = format!("{key}={value}");
        } else {
            lines.push(format!("{key}={value}"));
        }
    };

    if let Some(v) = o.ofa_enabled {
        set("ofa_enabled", (v as i32).to_string());
    }
    if let Some(v) = o.ofa_grid {
        set("ofa_grid", v.to_string());
    }
    if let Some(v) = o.ofa_perf {
        set("ofa_perf", v.to_string());
    }
    if let Some(v) = o.engine_velocity {
        set("engine_velocity", (v as i32).to_string());
    }
    if let Some(v) = o.reset_mode {
        set("reset_mode", v.to_string());
        set("reset_every", if v == 1 { "1" } else { "0" }.into());
    }
    if let Some(v) = o.light_stab {
        set("light_stab", (v as i32).to_string());
    }
    if let Some(v) = o.light_stab_strength {
        set("light_stab_strength", format!("{v:.3}"));
    }
    if let Some(v) = o.light_stab_max_delta {
        set("light_stab_max_delta", format!("{v:.3}"));
    }
    if let Some(v) = o.work_resolution {
        set("work_resolution", v.clamp(50, 100).to_string());
    }
    if let Some(v) = o.auto_profile_applied {
        set("auto_profile_applied", v.to_string());
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// After Feeder cfg is written, apply a matching override (if any). Returns a log line.
pub fn apply_for_game(game_dir: &Path, exe: &Path) -> Result<Option<String>> {
    let Some(o) = find_override(exe) else {
        return Ok(None);
    };
    let path = game_dir.join("dlss5-feed.cfg");
    if !path.is_file() {
        return Ok(None);
    }
    let prev = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let next = apply_to_cfg_text(&prev, &o);
    fs::write(&path, next).with_context(|| format!("writing {}", path.display()))?;
    let note = if o.note.is_empty() {
        "game override applied".into()
    } else {
        format!("game override: {}", o.note)
    };
    Ok(Some(note))
}

/// Generic RT-likely seed: slightly stronger LightStab (does not require a named override).
pub fn apply_rt_likely_seed(game_dir: &Path) -> Result<()> {
    let path = game_dir.join("dlss5-feed.cfg");
    if !path.is_file() {
        return Ok(());
    }
    let o = GameOverride {
        light_stab: Some(true),
        light_stab_strength: Some(0.40),
        light_stab_max_delta: Some(0.08),
        work_resolution: Some(90),
        auto_profile_applied: Some(0),
        note: "rt_likely".into(),
        ..Default::default()
    };
    let prev = fs::read_to_string(&path)?;
    let next = apply_to_cfg_text(&prev, &o);
    fs::write(&path, next)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_gothic_by_path() {
        let exe = PathBuf::from(r"D:\Games\Gothic 3\Gothic3.exe");
        let o = find_override(&exe).expect("gothic override");
        assert_eq!(o.work_resolution, Some(100));
        assert_eq!(o.reset_mode, Some(1));
    }

    #[test]
    fn matches_sims_by_exe() {
        let exe = PathBuf::from(r"D:\torrent\The Sims 4\Game\Bin\TS4_x64.exe");
        let o = find_override(&exe).expect("sims override");
        assert_eq!(o.work_resolution, Some(75));
    }

    #[test]
    fn apply_patches_cfg_lines() {
        let o = GameOverride {
            work_resolution: Some(75),
            reset_mode: Some(1),
            light_stab: Some(true),
            ..Default::default()
        };
        let cfg = "enabled=1\nwork_resolution=100\nofa_grid=1\n";
        let out = apply_to_cfg_text(cfg, &o);
        assert!(out.contains("work_resolution=75"));
        assert!(out.contains("reset_mode=1"));
        assert!(out.contains("light_stab=1"));
    }
}
