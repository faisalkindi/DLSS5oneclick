//! Offline read/write of `dlss5-feed.cfg` + `DLSS5_Feed.fx` uniforms in ReShadePreset.ini.

use crate::quality_preset;
use crate::reshade_ini::{self, Ini};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const CFG_NAME: &str = "dlss5-feed.cfg";

/// Editable Feeder knobs + FX residual masks — what the Setup panel shows.
#[derive(Debug, Clone, PartialEq)]
pub struct FeederKnobs {
    pub work_resolution: i32,
    pub work_upscale: i32,
    pub work_sharpness: f32,
    pub ofa_enabled: bool,
    pub ofa_grid: i32,
    pub ofa_perf: i32,
    pub reset_mode: i32,
    pub light_stab: bool,
    pub light_stab_strength: f32,
    pub engine_velocity: bool,
    pub evaluate_stride: i32,
    pub log_detail: i32,
    pub appearance_mask: bool,
    pub lighting_mask: bool,
    pub detail_mask: bool,
    pub appearance_threshold: f32,
    pub lighting_threshold: f32,
    pub detail_threshold: f32,
    pub auto_profile: String,
}

impl Default for FeederKnobs {
    /// Match Feeder stock (`g_cfg` + OFA/velocity/lightstab/diag + FX shader defaults).
    fn default() -> Self {
        Self {
            work_resolution: 100,
            work_upscale: 0,
            work_sharpness: 0.30,
            ofa_enabled: false,
            ofa_grid: 2,
            ofa_perf: 10, // feed_ofa.h: { enabled=0, grid=2, perf=10 }
            reset_mode: 2,
            light_stab: false,
            light_stab_strength: 0.35,
            engine_velocity: true,
            evaluate_stride: 1,
            log_detail: 1,
            appearance_mask: true,
            lighting_mask: true,
            detail_mask: true,
            // DLSS5_Feed.fx uniform defaults
            appearance_threshold: 0.035,
            lighting_threshold: 0.040,
            detail_threshold: 0.018,
            auto_profile: String::new(),
        }
    }
}

impl FeederKnobs {
    /// Axes used for nearest-neighbor FPS lookup (normalized 0..1 later).
    pub fn knobs_vec(&self) -> [f32; 6] {
        [
            self.work_resolution as f32 / 100.0,
            if self.ofa_enabled { 1.0 } else { 0.0 },
            self.ofa_grid as f32 / 4.0,
            self.reset_mode as f32 / 2.0,
            if self.light_stab { 1.0 } else { 0.0 },
            self.evaluate_stride as f32 / 4.0,
        ]
    }
}

pub fn cfg_path(game_dir: &Path) -> PathBuf {
    game_dir.join(CFG_NAME)
}

#[allow(dead_code)]
pub fn exists(game_dir: &Path) -> bool {
    cfg_path(game_dir).is_file()
}

fn parse_kv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let s = l.trim();
            if s.is_empty() || s.starts_with('#') || s.starts_with(';') {
                return None;
            }
            let (k, v) = s.split_once('=')?;
            Some((k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect()
}

fn get_i(map: &[(String, String)], key: &str, default: i32) -> i32 {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

fn get_f(map: &[(String, String)], key: &str, default: f32) -> f32 {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

fn get_s(map: &[(String, String)], key: &str) -> String {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

fn fx_f(ini: &Ini, key: &str, default: f32) -> f32 {
    ini.get("DLSS5_Feed.fx", key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn fx_b(ini: &Ini, key: &str, default: bool) -> bool {
    match ini.get("DLSS5_Feed.fx", key) {
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        None => default,
    }
}

/// Load knobs from disk. Err when cfg is missing.
pub fn load(game_dir: &Path) -> Result<FeederKnobs> {
    let path = cfg_path(game_dir);
    if !path.is_file() {
        bail!("not installed: {} missing", CFG_NAME);
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let kv = parse_kv(&text);
    let preset = Ini::load(&game_dir.join("ReShadePreset.ini"));
    let mut k = FeederKnobs::default();
    k.work_resolution = get_i(&kv, "work_resolution", k.work_resolution).clamp(50, 100);
    k.work_upscale = get_i(&kv, "work_upscale", k.work_upscale);
    k.work_sharpness = get_f(&kv, "work_sharpness", k.work_sharpness);
    k.ofa_enabled = get_i(&kv, "ofa_enabled", 0) != 0;
    k.ofa_grid = get_i(&kv, "ofa_grid", k.ofa_grid);
    k.ofa_perf = get_i(&kv, "ofa_perf", k.ofa_perf);
    k.reset_mode = get_i(&kv, "reset_mode", k.reset_mode);
    k.light_stab = get_i(&kv, "light_stab", 0) != 0;
    k.light_stab_strength = get_f(&kv, "light_stab_strength", k.light_stab_strength);
    k.engine_velocity = get_i(&kv, "engine_velocity", 1) != 0;
    k.evaluate_stride = get_i(&kv, "evaluate_stride", k.evaluate_stride).clamp(1, 4);
    k.log_detail = get_i(&kv, "log_detail", k.log_detail);
    k.auto_profile = get_s(&kv, "auto_profile");
    k.appearance_mask = fx_b(&preset, "APPEARANCE_MASK", k.appearance_mask);
    k.lighting_mask = fx_b(&preset, "LIGHTING_MASK", k.lighting_mask);
    k.detail_mask = fx_b(&preset, "DETAIL_MASK", k.detail_mask);
    k.appearance_threshold = fx_f(&preset, "APPEARANCE_THRESHOLD", k.appearance_threshold);
    k.lighting_threshold = fx_f(&preset, "LIGHTING_THRESHOLD", k.lighting_threshold);
    k.detail_threshold = fx_f(&preset, "DETAIL_THRESHOLD", k.detail_threshold);
    Ok(k)
}

fn set_line(lines: &mut Vec<String>, key: &str, value: String) {
    let prefix = format!("{key}=");
    if let Some(i) = lines.iter().position(|l| {
        l.to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    }) {
        lines[i] = format!("{key}={value}");
    } else {
        lines.push(format!("{key}={value}"));
    }
}

/// Write knobs back to cfg + FX uniforms. Feeder re-reads cfg ~every 60 frames.
pub fn save(game_dir: &Path, k: &FeederKnobs) -> Result<()> {
    let path = cfg_path(game_dir);
    let prev = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        // Seed a full cfg from medium defaults so Feeder has every key.
        let r = quality_preset::fallback_medium();
        quality_preset::feeder_cfg_text(&r)
    };
    let mut lines: Vec<String> = prev.lines().map(|l| l.to_string()).collect();
    set_line(
        &mut lines,
        "work_resolution",
        k.work_resolution.clamp(50, 100).to_string(),
    );
    set_line(&mut lines, "work_upscale", k.work_upscale.to_string());
    set_line(
        &mut lines,
        "work_sharpness",
        format!("{:.2}", k.work_sharpness),
    );
    set_line(
        &mut lines,
        "ofa_enabled",
        (k.ofa_enabled as i32).to_string(),
    );
    set_line(&mut lines, "ofa_grid", k.ofa_grid.to_string());
    set_line(&mut lines, "ofa_perf", k.ofa_perf.to_string());
    set_line(&mut lines, "reset_mode", k.reset_mode.to_string());
    set_line(
        &mut lines,
        "reset_every",
        if k.reset_mode == 1 { "1" } else { "0" }.into(),
    );
    set_line(&mut lines, "light_stab", (k.light_stab as i32).to_string());
    set_line(
        &mut lines,
        "light_stab_strength",
        format!("{:.3}", k.light_stab_strength),
    );
    set_line(
        &mut lines,
        "engine_velocity",
        (k.engine_velocity as i32).to_string(),
    );
    set_line(
        &mut lines,
        "evaluate_stride",
        k.evaluate_stride.clamp(1, 4).to_string(),
    );
    set_line(&mut lines, "log_detail", k.log_detail.to_string());
    // Manual edit → mark custom so auto-profile does not fight the user.
    set_line(&mut lines, "quality_preset", "custom".into());
    set_line(&mut lines, "auto_profile_applied", "1".into());
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;

    let fx = vec![
        (
            "APPEARANCE_MASK",
            if k.appearance_mask { "1" } else { "0" }.into(),
        ),
        (
            "APPEARANCE_THRESHOLD",
            format!("{:.3}", k.appearance_threshold),
        ),
        (
            "LIGHTING_MASK",
            if k.lighting_mask { "1" } else { "0" }.into(),
        ),
        ("LIGHTING_THRESHOLD", format!("{:.3}", k.lighting_threshold)),
        ("DETAIL_MASK", if k.detail_mask { "1" } else { "0" }.into()),
        ("DETAIL_THRESHOLD", format!("{:.3}", k.detail_threshold)),
    ];
    reshade_ini::write_feed_fx_uniforms(game_dir, &fx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_cfg() {
        let t = tempdir().unwrap();
        let d = t.path();
        let k = FeederKnobs {
            work_resolution: 85,
            ofa_enabled: true,
            reset_mode: 1,
            appearance_threshold: 0.033,
            ..Default::default()
        };
        save(d, &k).unwrap();
        assert!(exists(d));
        let back = load(d).unwrap();
        assert_eq!(back.work_resolution, 85);
        assert!(back.ofa_enabled);
        assert_eq!(back.reset_mode, 1);
        assert!((back.appearance_threshold - 0.033).abs() < 0.001);
    }

    #[test]
    fn missing_cfg_errors() {
        let t = tempdir().unwrap();
        assert!(load(t.path()).is_err());
    }
}
