//! Read Feeder `dlss5-perf.jsonl` and estimate FPS from nearest knob samples.

use crate::feeder_cfg::FeederKnobs;
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub const PERF_NAME: &str = "dlss5-perf.jsonl";
/// Need at least this many samples before trusting a nearest-neighbor estimate.
pub const MIN_SAMPLES: usize = 8;

#[derive(Debug, Clone, Deserialize)]
pub struct PerfSample {
    #[allow(dead_code)]
    pub ts: Option<u64>,
    pub fps: f32,
    #[allow(dead_code)]
    pub frame_ms: Option<f32>,
    #[allow(dead_code)]
    pub cpu_pct: Option<f32>,
    pub work_resolution: Option<i32>,
    pub ofa_enabled: Option<i32>,
    pub ofa_grid: Option<i32>,
    #[allow(dead_code)]
    pub ofa_perf: Option<i32>,
    pub reset_mode: Option<i32>,
    pub light_stab: Option<i32>,
    pub evaluate_stride: Option<i32>,
    #[allow(dead_code)]
    pub auto_profile: Option<String>,
}

impl PerfSample {
    fn knobs_vec(&self) -> [f32; 6] {
        [
            self.work_resolution.unwrap_or(100) as f32 / 100.0,
            if self.ofa_enabled.unwrap_or(0) != 0 {
                1.0
            } else {
                0.0
            },
            self.ofa_grid.unwrap_or(2) as f32 / 4.0,
            self.reset_mode.unwrap_or(2) as f32 / 2.0,
            if self.light_stab.unwrap_or(0) != 0 {
                1.0
            } else {
                0.0
            },
            self.evaluate_stride.unwrap_or(1) as f32 / 4.0,
        ]
    }
}

pub fn path(game_dir: &Path) -> PathBuf {
    game_dir.join(PERF_NAME)
}

pub fn load(game_dir: &Path) -> Vec<PerfSample> {
    let p = path(game_dir);
    let Ok(f) = fs::File::open(&p) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(s) = serde_json::from_str::<PerfSample>(t) {
            if s.fps.is_finite() && s.fps > 1.0 && s.fps < 1000.0 {
                out.push(s);
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct ExpectedFps {
    pub fps: f32,
    /// 0..1 confidence from neighbour count + distance.
    pub confidence: f32,
    pub neighbours: usize,
    #[allow(dead_code)]
    pub mean_distance: f32,
}

fn l1(a: &[f32; 6], b: &[f32; 6]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
}

/// Nearest-neighbour expected FPS for the current knobs.
/// Returns `None` when there are too few samples (caller should say "need in-game session").
pub fn expected_fps(samples: &[PerfSample], knobs: &FeederKnobs) -> Option<ExpectedFps> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let target = knobs.knobs_vec();
    let mut scored: Vec<(f32, f32)> = samples
        .iter()
        .map(|s| (l1(&target, &s.knobs_vec()), s.fps))
        .collect();
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let k = scored.len().clamp(1, 5);
    let neigh = &scored[..k];
    let mean_d = neigh.iter().map(|(d, _)| *d).sum::<f32>() / k as f32;
    // Inverse-distance weighting (epsilon so exact matches dominate).
    let mut wsum = 0.0f32;
    let mut fsum = 0.0f32;
    for (d, fps) in neigh {
        let w = 1.0 / (d + 0.05);
        wsum += w;
        fsum += w * fps;
    }
    let fps = fsum / wsum.max(1e-6);
    // Confidence: more neighbours + closer в†’ higher. Dist of 0.5 across 6 axes is "far".
    let dist_factor = (1.0 - (mean_d / 1.5).min(1.0)).max(0.0);
    let n_factor = (samples.len() as f32 / 40.0).min(1.0);
    let confidence = (0.55 * dist_factor + 0.45 * n_factor).clamp(0.0, 1.0);
    Some(ExpectedFps {
        fps,
        confidence,
        neighbours: k,
        mean_distance: mean_d,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(work: i32, ofa: i32, fps: f32) -> PerfSample {
        PerfSample {
            ts: None,
            fps,
            frame_ms: Some(1000.0 / fps),
            cpu_pct: Some(10.0),
            work_resolution: Some(work),
            ofa_enabled: Some(ofa),
            ofa_grid: Some(2),
            ofa_perf: Some(15),
            reset_mode: Some(2),
            light_stab: Some(0),
            evaluate_stride: Some(1),
            auto_profile: None,
        }
    }

    #[test]
    fn needs_enough_samples() {
        let knobs = FeederKnobs {
            work_resolution: 85,
            ..Default::default()
        };
        let few: Vec<_> = (0..3).map(|i| sample(85, 0, 60.0 + i as f32)).collect();
        assert!(expected_fps(&few, &knobs).is_none());
    }

    #[test]
    fn nearest_neighbour_prefers_close_work() {
        let mut samples = Vec::new();
        for i in 0..10 {
            samples.push(sample(70, 0, 90.0 + i as f32 * 0.1));
            samples.push(sample(100, 0, 50.0 + i as f32 * 0.1));
        }
        let knobs70 = FeederKnobs {
            work_resolution: 70,
            ..Default::default()
        };
        let e = expected_fps(&samples, &knobs70).unwrap();
        assert!(e.fps > 80.0, "got {}", e.fps);
        let knobs100 = FeederKnobs {
            work_resolution: 100,
            ..Default::default()
        };
        let e = expected_fps(&samples, &knobs100).unwrap();
        assert!(e.fps < 60.0, "got {}", e.fps);
    }
}
