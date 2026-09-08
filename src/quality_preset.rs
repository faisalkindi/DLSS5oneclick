//! Quality presets for Feeder installs: Auto / Low / Medium / High.
//!
//! Used only to write a sensible default `dlss5-feed.cfg` + FX uniforms at Install.
//! Live knobs live in the ReShade overlay (Feeder), not in the oneclick GUI.
//! Feeder auto-profile then retunes reset / lightstab / OFA once from observed game data.

use crate::game::{Api, GameStatus, Mode};
use crate::gpu::{self, Tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityChoice {
    Auto,
    Low,
    Medium,
    High,
}

impl QualityChoice {
    pub fn label(self) -> &'static str {
        match self {
            QualityChoice::Auto => "Auto",
            QualityChoice::Low => "Low",
            QualityChoice::Medium => "Medium",
            QualityChoice::High => "High",
        }
    }

    pub fn cfg_name(self) -> &'static str {
        match self {
            QualityChoice::Auto => "auto",
            QualityChoice::Low => "low",
            QualityChoice::Medium => "medium",
            QualityChoice::High => "high",
        }
    }
}

/// Optional Advanced overrides from the GUI (applied on top of the chosen preset).
#[derive(Debug, Clone, Default)]
pub struct QualityOverrides {
    pub ofa_enabled: Option<bool>,
    pub ofa_grid: Option<i32>,
    pub work_resolution: Option<i32>,
    pub work_upscale: Option<i32>,
    pub appearance_mask: Option<bool>,
    pub lighting_mask: Option<bool>,
    pub detail_mask: Option<bool>,
    pub appearance_threshold: Option<f32>,
    pub lighting_threshold: Option<f32>,
    pub detail_threshold: Option<f32>,
}

impl QualityOverrides {
    pub fn any_set(&self) -> bool {
        self.ofa_enabled.is_some()
            || self.ofa_grid.is_some()
            || self.work_resolution.is_some()
            || self.work_upscale.is_some()
            || self.appearance_mask.is_some()
            || self.lighting_mask.is_some()
            || self.detail_mask.is_some()
            || self.appearance_threshold.is_some()
            || self.lighting_threshold.is_some()
            || self.detail_threshold.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedQuality {
    pub choice: QualityChoice,
    /// True when Advanced overrides changed something vs the base preset.
    pub customized: bool,
    pub ofa_enabled: bool,
    pub ofa_grid: i32,
    pub ofa_perf: i32,
    pub work_resolution: i32,
    pub work_upscale: i32,
    pub work_sharpness: f32,
    pub engine_velocity: bool,
    /// Enable Lumenite_Kernel technique in ReShadePreset (false when OFA supplies MV).
    pub enable_lumenite: bool,
    pub appearance_mask: bool,
    pub lighting_mask: bool,
    pub detail_mask: bool,
    pub appearance_threshold: f32,
    pub appearance_strength: f32,
    pub lighting_threshold: f32,
    pub lighting_strength: f32,
    pub detail_threshold: f32,
    pub detail_strength: f32,
    pub summary: String,
}

fn nvidia_ofa_capable(st: &GameStatus) -> bool {
    let tier = st
        .gpu
        .as_ref()
        .map(|(_, t)| *t)
        .or_else(|| gpu::best().map(|(_, t)| t));
    match tier {
        Some(Tier::Rtx50 | Tier::Rtx40 | Tier::Rtx2030) => true,
        Some(Tier::Unknown) => true, // named NVIDIA without clear RTX digits
        _ => false,
    }
}

fn base_low() -> ResolvedQuality {
    ResolvedQuality {
        choice: QualityChoice::Low,
        customized: false,
        ofa_enabled: false,
        ofa_grid: 2,
        ofa_perf: 10,
        work_resolution: 70,
        work_upscale: 1,
        work_sharpness: 0.35,
        engine_velocity: false,
        enable_lumenite: true,
        appearance_mask: true,
        lighting_mask: true,
        detail_mask: true,
        appearance_threshold: 0.050,
        appearance_strength: 1.0,
        lighting_threshold: 0.060,
        lighting_strength: 1.0,
        detail_threshold: 0.028,
        detail_strength: 1.0,
        summary: "Low: Lumenite MV, softer masks, work 70% + FSR expand".into(),
    }
}

fn base_medium() -> ResolvedQuality {
    ResolvedQuality {
        choice: QualityChoice::Medium,
        customized: false,
        ofa_enabled: false,
        ofa_grid: 2,
        ofa_perf: 10,
        work_resolution: 85,
        work_upscale: 1,
        work_sharpness: 0.30,
        engine_velocity: true,
        enable_lumenite: true,
        appearance_mask: true,
        lighting_mask: true,
        detail_mask: true,
        appearance_threshold: 0.035,
        appearance_strength: 1.25,
        lighting_threshold: 0.040,
        lighting_strength: 1.35,
        detail_threshold: 0.018,
        detail_strength: 1.40,
        summary: "Medium: engine velocity hunt + Lumenite fallback, residual masks, work 85% + FSR"
            .into(),
    }
}

/// Safe default when no install context is set (tests / unexpected path).
pub fn fallback_medium() -> ResolvedQuality {
    base_medium()
}

fn base_high() -> ResolvedQuality {
    ResolvedQuality {
        choice: QualityChoice::High,
        customized: false,
        ofa_enabled: true,
        ofa_grid: 1,
        ofa_perf: 10,
        work_resolution: 100,
        work_upscale: 0,
        work_sharpness: 0.30,
        engine_velocity: true,
        enable_lumenite: false,
        appearance_mask: true,
        lighting_mask: true,
        detail_mask: true,
        appearance_threshold: 0.025,
        appearance_strength: 1.40,
        lighting_threshold: 0.030,
        lighting_strength: 1.50,
        detail_threshold: 0.012,
        detail_strength: 1.55,
        summary: "High: engine velocity + Optical Flow fallback, stronger masks, full resolution"
            .into(),
    }
}

fn apply_overrides(mut r: ResolvedQuality, o: &QualityOverrides) -> ResolvedQuality {
    if !o.any_set() {
        return r;
    }
    r.customized = true;
    if let Some(v) = o.ofa_enabled {
        r.ofa_enabled = v;
        r.enable_lumenite = !v;
    }
    if let Some(v) = o.ofa_grid {
        r.ofa_grid = v;
    }
    if let Some(v) = o.work_resolution {
        r.work_resolution = v.clamp(50, 100);
    }
    if let Some(v) = o.work_upscale {
        r.work_upscale = v.clamp(0, 2);
    }
    if let Some(v) = o.appearance_mask {
        r.appearance_mask = v;
    }
    if let Some(v) = o.lighting_mask {
        r.lighting_mask = v;
    }
    if let Some(v) = o.detail_mask {
        r.detail_mask = v;
    }
    if let Some(v) = o.appearance_threshold {
        r.appearance_threshold = v;
    }
    if let Some(v) = o.lighting_threshold {
        r.lighting_threshold = v;
    }
    if let Some(v) = o.detail_threshold {
        r.detail_threshold = v;
    }
    r.summary = format!("{} (customized)", r.choice.label());
    r
}

/// Resolve a quality choice for this game / machine.
pub fn resolve(
    choice: QualityChoice,
    st: &GameStatus,
    overrides: &QualityOverrides,
) -> ResolvedQuality {
    let ofa_ok = st.mode == Mode::Feeder
        && matches!(st.api, Api::Dx11)
        && !st.is32()
        && st.anticheat.is_none()
        && nvidia_ofa_capable(st);

    let base = match choice {
        QualityChoice::Low => base_low(),
        QualityChoice::Medium => base_medium(),
        QualityChoice::High => {
            let mut h = base_high();
            if !ofa_ok {
                // High without OFA: keep aggressive masks + velocity hunt, fall back to Lumenite.
                h.ofa_enabled = false;
                h.enable_lumenite = true;
                h.engine_velocity = true;
                h.summary =
                    "High: engine velocity + Lumenite (Optical Flow unavailable), stronger masks, full res"
                        .into();
            }
            h
        }
        QualityChoice::Auto => {
            if ofa_ok {
                let mut a = base_high();
                a.choice = QualityChoice::Auto;
                a.ofa_grid = 2; // Auto uses default grid, not High's 1px
                a.engine_velocity = true;
                a.appearance_threshold = 0.030;
                a.lighting_threshold = 0.035;
                a.detail_threshold = 0.015;
                a.summary =
                    "Auto: engine velocity + Optical Flow + residual masks (Feeder D3D11 + RTX) — recommendation only"
                        .into();
                a
            } else if matches!(st.api, Api::Dx12) {
                let mut a = base_medium();
                a.choice = QualityChoice::Auto;
                a.ofa_enabled = false;
                a.enable_lumenite = true;
                a.engine_velocity = true;
                a.work_resolution = 100;
                a.summary =
                    "Auto: DX12 Feeder (no OFA) — Lumenite MV + velocity hunt; Optimize uses half-rate cost first"
                        .into();
                a
            } else {
                let mut a = base_medium();
                a.choice = QualityChoice::Auto;
                a.engine_velocity = true;
                a.summary =
                    "Auto: Medium + engine velocity hunt (Optical Flow not applicable) — recommendation only"
                        .into();
                a
            }
        }
    };

    let mut r = apply_overrides(base, overrides);
    // RT-likely titles: slightly cheaper seed + stronger light stab (flicker proxy).
    if st.mode == Mode::Feeder && st.rt_likely && !r.customized {
        r.work_resolution = r.work_resolution.min(90);
        r.summary = format!("{} · RT-likely seed (work≤90%)", r.summary);
    }
    r
}

/// Full `dlss5-feed.cfg` text matching Feeder's CfgSave layout (defaults for untouched keys).
/// `auto_profile_applied=0` so the add-on auto-tunes once on first attach from live game data.
pub fn feeder_cfg_text(r: &ResolvedQuality) -> String {
    format!(
        "enabled=1\n\
         mode=2\n\
         hdr=-1\n\
         depth_inverted=-1\n\
         flags=-1\n\
         reset_every=0\n\
         warmup_rebuild=180\n\
         rebuild=0\n\
         log_frames=3\n\
         create_delay=60\n\
         preset=0\n\
         work_resolution={}\n\
         work_upscale={}\n\
         work_sharpness={:.2}\n\
         gpu_timeout_ms=2000\n\
         buffer_home=1\n\
         async_home=0\n\
         sync_home=0\n\
         mv_scale_x=1.000\n\
         mv_scale_y=1.000\n\
         stall_log_ms=50\n\
         reset_mode=2\n\
         log_detail=1\n\
         log_detail_every=60\n\
         light_stab=0\n\
         light_stab_strength=0.350\n\
         light_stab_max_delta=0.060\n\
         ofa_enabled={}\n\
         ofa_grid={}\n\
         ofa_perf={}\n\
         engine_velocity={}\n\
         velocity_cand=-1\n\
         velocity_decode=0\n\
         velocity_scale=1.000\n\
         early_color=0\n\
         early_color_cand=-1\n\
         async_feed=0\n\
         key_async_feed=121\n\
         hud_stats=0\n\
         key_hud=120\n\
         quality_preset={}\n\
         auto_profile_applied=0\n\
         auto_profile=\n",
        r.work_resolution,
        r.work_upscale,
        r.work_sharpness,
        if r.ofa_enabled { 1 } else { 0 },
        r.ofa_grid,
        r.ofa_perf,
        if r.engine_velocity { 1 } else { 0 },
        if r.customized {
            "custom"
        } else {
            r.choice.cfg_name()
        },
    )
}

/// Uniform keys for `[DLSS5_Feed.fx]` in ReShadePreset.ini.
pub fn feed_fx_uniforms(r: &ResolvedQuality) -> Vec<(&'static str, String)> {
    vec![
        (
            "APPEARANCE_MASK",
            if r.appearance_mask { "1" } else { "0" }.into(),
        ),
        (
            "APPEARANCE_THRESHOLD",
            format!("{:.3}", r.appearance_threshold),
        ),
        (
            "APPEARANCE_STRENGTH",
            format!("{:.2}", r.appearance_strength),
        ),
        (
            "LIGHTING_MASK",
            if r.lighting_mask { "1" } else { "0" }.into(),
        ),
        ("LIGHTING_THRESHOLD", format!("{:.3}", r.lighting_threshold)),
        ("LIGHTING_STRENGTH", format!("{:.2}", r.lighting_strength)),
        ("DETAIL_MASK", if r.detail_mask { "1" } else { "0" }.into()),
        ("DETAIL_THRESHOLD", format!("{:.3}", r.detail_threshold)),
        ("DETAIL_STRENGTH", format!("{:.2}", r.detail_strength)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{Api, Mode};
    use std::path::PathBuf;

    fn stub_status(mode: Mode, api: Api) -> GameStatus {
        GameStatus {
            mode,
            api,
            bridge: false,
            opti: false,
            gpu: Some((
                crate::gpu::Gpu {
                    name: "NVIDIA GeForce RTX 4060".into(),
                    vendor: "NVIDIA".into(),
                },
                Tier::Rtx40,
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
            host_exe: false,
            host_reshade: false,
            re_engine: false,
            reframework: false,
            upstream: false,
            unreal_likely: false,
            unity_likely: false,
            rt_likely: false,
            renodx_mod: None,
            foreign_renodx: vec![],
            anticheat: None,
            problems: vec![],
        }
    }

    #[test]
    fn auto_d3d11_rtx_enables_ofa() {
        let st = stub_status(Mode::Feeder, Api::Dx11);
        let r = resolve(QualityChoice::Auto, &st, &QualityOverrides::default());
        assert!(r.ofa_enabled);
        assert!(!r.enable_lumenite);
    }

    #[test]
    fn auto_dx12_falls_back_medium() {
        let st = stub_status(Mode::Feeder, Api::Dx12);
        let r = resolve(QualityChoice::Auto, &st, &QualityOverrides::default());
        assert!(!r.ofa_enabled);
        assert!(r.enable_lumenite);
        assert_eq!(r.work_resolution, 100);
        assert!(r.summary.contains("DX12"));
    }

    #[test]
    fn medium_baseline_keys() {
        let r = base_medium();
        assert!(!r.ofa_enabled);
        assert!(r.enable_lumenite);
        assert!(r.engine_velocity);
        assert_eq!(r.work_resolution, 85);
        let cfg = feeder_cfg_text(&r);
        assert!(cfg.contains("ofa_enabled=0"));
        assert!(cfg.contains("engine_velocity=1"));
        assert!(cfg.contains("work_resolution=85"));
        assert!(cfg.contains("quality_preset=medium"));
        assert!(cfg.contains("auto_profile_applied=0"));
    }

    #[test]
    fn high_aggressive_masks() {
        let r = base_high();
        assert!(r.ofa_enabled);
        assert!(!r.enable_lumenite);
        assert!(r.engine_velocity);
        assert!(r.appearance_threshold < base_medium().appearance_threshold);
        let fx = feed_fx_uniforms(&r);
        assert!(fx.iter().any(|(k, v)| *k == "DETAIL_MASK" && v == "1"));
    }

    #[test]
    fn overrides_mark_custom() {
        let o = QualityOverrides {
            work_resolution: Some(90),
            ..Default::default()
        };
        let r = apply_overrides(base_medium(), &o);
        assert!(r.customized);
        assert_eq!(r.work_resolution, 90);
        assert!(feeder_cfg_text(&r).contains("quality_preset=custom"));
    }
}
