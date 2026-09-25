//! Global app settings: `%LOCALAPPDATA%\dlss5oneclick\settings.json`.
//!
//! These are **user install defaults**: saved here and applied on new Install
//! (and optionally "Apply defaults to this game"). They are distinct from
//! **Feeder built-in defaults** (`CfgWriteDefault` / static `g_cfg` in
//! DLSS5-Feeder) — use [`Settings::feeder_stock`] / the Settings UI
//! "Reset to Feeder defaults" button to restore the form to Feeder-like values.

use crate::quality_preset::{QualityChoice, QualityOverrides};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Seed quality for Feeder installs.
    #[serde(default = "default_quality")]
    pub quality: String,
    /// Feeder overlay / cfg defaults written on Install.
    #[serde(default)]
    pub knobs: KnobDefaults,
    /// Soft overlay UX defaults (cfg keys the Feeder reads).
    #[serde(default)]
    pub overlay: OverlayDefaults,
    /// The Setup page opens with the advanced options shown (#77).
    #[serde(default)]
    pub advanced_open: bool,
}

fn default_quality() -> String {
    "auto".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnobDefaults {
    pub work_resolution: Option<i32>,
    pub ofa_enabled: Option<bool>,
    pub ofa_grid: Option<i32>,
    pub ofa_perf: Option<i32>,
    pub reset_mode: Option<i32>,
    pub light_stab: Option<bool>,
    pub engine_velocity: Option<bool>,
    pub appearance_mask: Option<bool>,
    pub lighting_mask: Option<bool>,
    pub detail_mask: Option<bool>,
    pub appearance_threshold: Option<f32>,
    pub lighting_threshold: Option<f32>,
    pub detail_threshold: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayDefaults {
    #[serde(default = "default_log_detail")]
    pub log_detail: i32,
    #[serde(default = "default_evaluate_stride")]
    pub evaluate_stride: i32,
    #[serde(default = "default_log_frames")]
    pub log_frames: i32,
}

fn default_log_detail() -> i32 {
    1
}
fn default_evaluate_stride() -> i32 {
    1
}
fn default_log_frames() -> i32 {
    3
}

impl Default for OverlayDefaults {
    fn default() -> Self {
        Self {
            log_detail: default_log_detail(),
            evaluate_stride: default_evaluate_stride(),
            log_frames: default_log_frames(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            quality: default_quality(),
            knobs: KnobDefaults::default(),
            overlay: OverlayDefaults::default(),
            advanced_open: false,
        }
    }
}

impl Settings {
    /// Feeder stock values (`dlss5-feed.cpp` `g_cfg` + OFA/velocity/lightstab/diag headers).
    ///
    /// Typical stock cfg / UI:
    /// - `quality` seed = Auto (oneclick only; Feeder has no quality seed file)
    /// - `work_resolution` = 100
    /// - `ofa_enabled` = off, `ofa_grid` = 2, `ofa_perf` = 10
    /// - `reset_mode` = 2 (adaptive), `light_stab` = off, `engine_velocity` = on
    /// - overlay: `log_detail` = 1, `evaluate_stride` = 1, `log_frames` = 3
    ///
    /// Residual FX mask toggles/thresholds stay unset (`None`) so Install still
    /// follows the quality preset for those axes.
    pub fn feeder_stock() -> Self {
        Self {
            quality: default_quality(),
            knobs: KnobDefaults {
                work_resolution: Some(100),
                ofa_enabled: Some(false),
                ofa_grid: Some(2),
                ofa_perf: Some(10),
                reset_mode: Some(2),
                light_stab: Some(false),
                engine_velocity: Some(true),
                appearance_mask: None,
                lighting_mask: None,
                detail_mask: None,
                appearance_threshold: None,
                lighting_threshold: None,
                detail_threshold: None,
            },
            overlay: OverlayDefaults {
                log_detail: 1,
                evaluate_stride: 1,
                log_frames: 3,
            },
            advanced_open: false,
        }
    }

    /// Restore knobs / overlay / quality seed to [`Self::feeder_stock`].
    pub fn reset_to_feeder_defaults(&mut self) {
        // Only the Feeder values: whether advanced options are shown is a
        // window preference, not a Feeder default.
        let advanced_open = self.advanced_open;
        *self = Self::feeder_stock();
        self.advanced_open = advanced_open;
    }

    pub fn path() -> PathBuf {
        dirs_local_appdata()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dlss5oneclick")
            .join("settings.json")
    }

    pub fn load() -> Self {
        let p = Self::path();
        match fs::read_to_string(&p) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let p = Self::path();
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let t = serde_json::to_string_pretty(self)?;
        fs::write(&p, t).with_context(|| format!("write {}", p.display()))?;
        Ok(())
    }

    pub fn quality_choice(&self) -> QualityChoice {
        match self.quality.to_ascii_lowercase().as_str() {
            "low" => QualityChoice::Low,
            "medium" | "med" => QualityChoice::Medium,
            "high" => QualityChoice::High,
            _ => QualityChoice::Auto,
        }
    }

    pub fn set_quality_choice(&mut self, c: QualityChoice) {
        self.quality = c.cfg_name().to_owned();
    }

    pub fn quality_overrides(&self) -> QualityOverrides {
        QualityOverrides {
            ofa_enabled: self.knobs.ofa_enabled,
            ofa_grid: self.knobs.ofa_grid,
            work_resolution: self.knobs.work_resolution,
            work_upscale: None,
            appearance_mask: self.knobs.appearance_mask,
            lighting_mask: self.knobs.lighting_mask,
            detail_mask: self.knobs.detail_mask,
            appearance_threshold: self.knobs.appearance_threshold,
            lighting_threshold: self.knobs.lighting_threshold,
            detail_threshold: self.knobs.detail_threshold,
        }
    }
}

fn dirs_local_appdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

/// Patch cfg text with overlay UX defaults from settings (log_detail, evaluate_stride, …).
pub fn apply_overlay_to_cfg(cfg: &str, s: &Settings) -> String {
    let mut lines: Vec<String> = cfg.lines().map(|l| l.to_string()).collect();
    let mut set = |key: &str, value: String| {
        let prefix = format!("{key}=");
        if let Some(i) = lines.iter().position(|l| {
            l.to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        }) {
            lines[i] = format!("{key}={value}");
        } else {
            lines.push(format!("{key}={value}"));
        }
    };
    set("log_detail", s.overlay.log_detail.to_string());
    set(
        "evaluate_stride",
        s.overlay.evaluate_stride.clamp(1, 4).to_string(),
    );
    set("log_frames", s.overlay.log_frames.to_string());
    if let Some(v) = s.knobs.reset_mode {
        set("reset_mode", v.to_string());
        set("reset_every", if v == 1 { "1" } else { "0" }.into());
    }
    if let Some(v) = s.knobs.light_stab {
        set("light_stab", (v as i32).to_string());
    }
    if let Some(v) = s.knobs.engine_velocity {
        set("engine_velocity", (v as i32).to_string());
    }
    if let Some(v) = s.knobs.ofa_perf {
        set("ofa_perf", v.to_string());
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_defaults() {
        let s = Settings::default();
        assert_eq!(s.quality_choice(), QualityChoice::Auto);
        let t = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&t).unwrap();
        assert_eq!(back.quality, "auto");
        assert_eq!(back.overlay.evaluate_stride, 1);
    }

    #[test]
    fn feeder_stock_matches_documented_knobs() {
        let s = Settings::feeder_stock();
        assert_eq!(s.quality_choice(), QualityChoice::Auto);
        assert_eq!(s.knobs.work_resolution, Some(100));
        assert_eq!(s.knobs.ofa_enabled, Some(false));
        assert_eq!(s.knobs.ofa_grid, Some(2));
        assert_eq!(s.knobs.ofa_perf, Some(10));
        assert_eq!(s.knobs.reset_mode, Some(2));
        assert_eq!(s.knobs.light_stab, Some(false));
        assert_eq!(s.knobs.engine_velocity, Some(true));
        assert_eq!(s.overlay.log_detail, 1);
        assert_eq!(s.overlay.evaluate_stride, 1);
        assert_eq!(s.overlay.log_frames, 3);
        let mut other = Settings {
            quality: "high".into(),
            knobs: KnobDefaults {
                work_resolution: Some(70),
                ..Default::default()
            },
            ..Default::default()
        };
        other.reset_to_feeder_defaults();
        assert_eq!(other.knobs.work_resolution, Some(100));
        assert_eq!(other.quality, "auto");
    }

    #[test]
    fn overlay_patch_inserts_keys() {
        let mut s = Settings::default();
        s.overlay.log_detail = 2;
        s.overlay.evaluate_stride = 2;
        let out = apply_overlay_to_cfg("enabled=1\nwork_resolution=100\n", &s);
        assert!(out.contains("log_detail=2"));
        assert!(out.contains("evaluate_stride=2"));
        assert!(out.contains("work_resolution=100"));
    }
}
