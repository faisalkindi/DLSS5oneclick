//! The setup this game gets without anyone choosing one, and what to try next.
//!
//! Every game has a ladder: the best setup first, then what to fall back to if
//! it does not work there. Install uses the rung the game is on; "Try the next
//! setup" moves it one rung down and installs that. The rung is recorded in the
//! game folder, so the choice follows the game rather than the session.
//!
//! The order comes from what is known about each route:
//! - ShortFuse's add-on is the RenoDX author's own neural consumer, and RHI
//!   recommends it for games that ship their own DLSS, so it is first there.
//! - The RenoDX DLSS 5 add-on, newest stable build, then 4.70: the build every
//!   reporter had working when 5.2.1 broke four games (#96, #86, #76, #100),
//!   and the last one with Enable Upscaling (#109).
//! - The OptiScaler engine last: a different engine altogether, and the one
//!   that fixed Cyberpunk 2077 (#95).
//!
//! Games with a known answer go straight to it (`PROFILES`).

use std::fs;
use std::path::Path;

use crate::game::{Api, GameStatus, Mode};
use crate::installer::{self, Consumer, Engine};

/// The rung this game is on, as a number, beside the exe.
pub const LEVEL_FILE: &str = ".dlss5oneclick-setup";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub engine: Engine,
    pub consumer: Consumer,
    /// A pinned DLSS 5 add-on build; `None` is the newest stable one.
    pub addon_tag: Option<&'static str>,
}

const SF: Setup = Setup {
    engine: Engine::ReShade,
    consumer: Consumer::ShortFuse,
    addon_tag: None,
};
const DLSS5_NEWEST: Setup = Setup {
    engine: Engine::ReShade,
    consumer: Consumer::Dlss5,
    addon_tag: None,
};
const DLSS5_STEADY: Setup = Setup {
    engine: Engine::ReShade,
    consumer: Consumer::Dlss5,
    addon_tag: Some(installer::RENODX_STEADY_TAG),
};
const DLSS5_CLASSIC: Setup = Setup {
    engine: Engine::ReShade,
    consumer: Consumer::Dlss5,
    addon_tag: Some(installer::RENODX_CLASSIC_TAG),
};
const OPTI: Setup = Setup {
    engine: Engine::Opti,
    consumer: Consumer::Dlss5,
    addon_tag: None,
};

/// Games whose reports settled the order, by exe name (lower case).
/// Cyberpunk 2077: the ReShade route churned its feature and changed nothing;
/// the OptiScaler engine worked for the reporter (#95). Dragon's Dogma 2: the
/// newer DLSS 5 add-on crashed at the first evaluate where the older build ran
/// (#96).
const PROFILES: &[(&str, &[Setup], &str)] = &[
    (
        "cyberpunk2077.exe",
        &[OPTI, SF, DLSS5_NEWEST, DLSS5_STEADY],
        "Cyberpunk 2077 works on the OptiScaler engine (#95)",
    ),
    (
        "dd2.exe",
        &[DLSS5_STEADY, SF, DLSS5_NEWEST, OPTI],
        "Dragon's Dogma 2 crashed on newer DLSS 5 add-on builds (#96)",
    ),
];

fn profile(st: &GameStatus) -> Option<(&'static [Setup], &'static str)> {
    let exe = st.exe.file_name()?.to_str()?.to_ascii_lowercase();
    PROFILES
        .iter()
        .find(|(n, ..)| *n == exe)
        .map(|(_, l, why)| (*l, *why))
}

/// Every setup worth trying in this game, best first. Empty when nothing this
/// tool offers fits (a Vulkan game without DLSS of its own).
pub fn ladder(st: &GameStatus) -> Vec<Setup> {
    if let Some((l, _)) = profile(st) {
        return l.to_vec();
    }
    match (st.mode, st.api) {
        // ReShade cannot reach Vulkan from dxgi.dll; OptiScaler can, when the
        // game has its own DLSS for it to read.
        (Mode::Native, Api::Vulkan) if !st.is32() => vec![OPTI],
        (_, Api::Vulkan) => vec![],
        (Mode::Native, _) if !st.is32() => vec![SF, DLSS5_NEWEST, DLSS5_STEADY, OPTI],
        // No DLSS of its own, or 32-bit: the Feeder carries it, and the DLSS 5
        // add-on is its consumer. 4.55 is the build the Feeder's host names as
        // passing where newer ones fault in the driver (#69).
        _ => vec![DLSS5_NEWEST, DLSS5_STEADY, DLSS5_CLASSIC],
    }
}

/// The rung the game is on. A game this tool set up before the ladder existed
/// has no file: an OptiScaler install stays on OptiScaler, anything else starts
/// at the top.
pub fn level(st: &GameStatus) -> usize {
    let l = ladder(st);
    let recorded = fs::read_to_string(st.game_dir().join(LEVEL_FILE))
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok());
    let n = match recorded {
        Some(n) => n,
        None if st.opti => l.iter().position(|s| s.engine == Engine::Opti).unwrap_or(0),
        None => 0,
    };
    n.min(l.len().saturating_sub(1))
}

/// A setup someone picked by hand before the ladder existed, which the picker
/// must not overwrite: Neural Upstream or the standalone AIO, with no rung on
/// record. Those stay under Advanced until the user changes them there.
pub fn hand_chosen(st: &GameStatus) -> bool {
    !st.game_dir().join(LEVEL_FILE).is_file() && (st.upstream || (st.aio && !st.opti))
}

/// The setup for this game now.
pub fn current(st: &GameStatus) -> Option<Setup> {
    ladder(st).get(level(st)).copied()
}

/// The next rung, when there is one.
pub fn next(st: &GameStatus) -> Option<(usize, Setup)> {
    let n = level(st) + 1;
    ladder(st).get(n).copied().map(|s| (n, s))
}

pub fn save_level(game_dir: &Path, n: usize) -> std::io::Result<()> {
    fs::write(game_dir.join(LEVEL_FILE), n.to_string())
}

/// Hand a setup to the installer: its consumer and add-on build go into the
/// environment the install steps read; the engine is returned for the caller.
pub fn apply(s: &Setup) -> Engine {
    std::env::set_var(
        installer::CONSUMER_ENV,
        match s.consumer {
            Consumer::ShortFuse => "sf",
            Consumer::Dlss5 => "dlss5",
        },
    );
    match s.addon_tag {
        Some(t) => std::env::set_var(installer::RENODX_TAG_ENV, t),
        None => std::env::remove_var(installer::RENODX_TAG_ENV),
    }
    s.engine
}

/// What the setup is, in words.
pub fn label(s: &Setup) -> String {
    match (s.engine, s.consumer, s.addon_tag) {
        (Engine::Opti, ..) => "OptiScaler with its built-in neural rendering pass".to_owned(),
        (Engine::Aio, ..) => "ReShade + standalone AIO".to_owned(),
        (_, Consumer::ShortFuse, _) => "ReShade + ShortFuse's DLSS add-on".to_owned(),
        (_, Consumer::Dlss5, None) => "ReShade + DLSS 5 add-on (newest stable build)".to_owned(),
        (_, Consumer::Dlss5, Some(t)) => format!(
            "ReShade + DLSS 5 add-on {}",
            t.trim_start_matches(installer::DLSS5_PREFIX)
        ),
    }
}

/// Why this game gets it: the facts the ladder was built from.
pub fn reason(st: &GameStatus) -> String {
    let mut parts = Vec::new();
    if let Some((_, why)) = profile(st) {
        parts.push(why.to_owned());
    }
    parts.push(
        match st.mode {
            Mode::Native => "the game has its own DLSS",
            Mode::Feeder => "the game has no DLSS of its own (the Feeder supplies it)",
        }
        .to_owned(),
    );
    parts.push(st.api.label().to_owned());
    if st.is32() {
        parts.push("32-bit".to_owned());
    }
    if let Some((_, tier)) = &st.gpu {
        parts.push(tier.label().to_owned());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{stub_status, Api, Mode};

    #[test]
    fn a_game_with_its_own_dlss_starts_on_shortfuse_and_falls_back_in_order() {
        let t = tempfile::tempdir().unwrap();
        let mut st = stub_status(Mode::Native, Api::Dx12);
        st.exe = t.path().join("game.exe");
        assert_eq!(ladder(&st), vec![SF, DLSS5_NEWEST, DLSS5_STEADY, OPTI]);
        assert_eq!(current(&st), Some(SF));
        assert_eq!(next(&st), Some((1, DLSS5_NEWEST)));
        save_level(t.path(), 2).unwrap();
        assert_eq!(current(&st), Some(DLSS5_STEADY));
        save_level(t.path(), 3).unwrap();
        assert_eq!(current(&st), Some(OPTI));
        assert_eq!(next(&st), None);
        // A number past the end stays on the last rung.
        save_level(t.path(), 9).unwrap();
        assert_eq!(current(&st), Some(OPTI));
    }

    #[test]
    fn feeder_32_bit_vulkan_and_profiles_get_their_own_ladders() {
        let t = tempfile::tempdir().unwrap();
        let mut st = stub_status(Mode::Feeder, Api::Dx11);
        st.exe = t.path().join("game.exe");
        assert_eq!(ladder(&st), vec![DLSS5_NEWEST, DLSS5_STEADY, DLSS5_CLASSIC]);
        st.mode = Mode::Native;
        st.bitness = 32;
        assert!(!ladder(&st).contains(&SF));
        st.bitness = 64;
        st.api = Api::Vulkan;
        assert_eq!(ladder(&st), vec![OPTI]);
        st.mode = Mode::Feeder;
        assert!(ladder(&st).is_empty());
        st.mode = Mode::Native;
        st.api = Api::Dx12;
        st.exe = t.path().join("Cyberpunk2077.exe");
        assert_eq!(current(&st), Some(OPTI));
        assert!(reason(&st).contains("#95"));
        st.exe = t.path().join("DD2.exe");
        assert_eq!(current(&st), Some(DLSS5_STEADY));
    }

    #[test]
    fn neural_upstream_and_aio_picked_by_hand_are_left_alone() {
        let t = tempfile::tempdir().unwrap();
        let mut st = stub_status(Mode::Native, Api::Dx12);
        st.exe = t.path().join("game.exe");
        assert!(!hand_chosen(&st));
        st.upstream = true;
        assert!(hand_chosen(&st));
        save_level(t.path(), 0).unwrap();
        assert!(!hand_chosen(&st));
    }

    #[test]
    fn an_optiscaler_install_from_before_the_ladder_stays_on_optiscaler() {
        let t = tempfile::tempdir().unwrap();
        let mut st = stub_status(Mode::Native, Api::Dx12);
        st.exe = t.path().join("game.exe");
        st.opti = true;
        assert_eq!(current(&st), Some(OPTI));
    }

    #[test]
    fn apply_hands_the_consumer_and_build_to_the_installer() {
        assert_eq!(apply(&DLSS5_STEADY), Engine::ReShade);
        assert_eq!(installer::consumer(), Consumer::Dlss5);
        assert_eq!(
            std::env::var(installer::RENODX_TAG_ENV).as_deref(),
            Ok(installer::RENODX_STEADY_TAG)
        );
        assert_eq!(apply(&SF), Engine::ReShade);
        assert_eq!(installer::consumer(), Consumer::ShortFuse);
        assert!(std::env::var(installer::RENODX_TAG_ENV).is_err());
        std::env::remove_var(installer::CONSUMER_ENV);
    }
}
