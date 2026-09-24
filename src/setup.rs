//! The setup this game gets without anyone choosing one, and what to try next.
//!
//! Every game has a ladder: the best setup first, then what to fall back to if
//! it does not work there. A game with nothing installed gets the top rung.
//! A game that is already set up is on the rung that matches what is in its
//! folder, read from the files themselves (the add-on and the build tag this
//! tool recorded beside it, or the OptiScaler manifest), so Install and Update
//! refresh what is there instead of moving it somewhere else. "Try the next
//! setup" is the only thing that moves a game down its ladder.
//!
//! The order comes from what is known about each route:
//! - ShortFuse's add-on is the RenoDX author's own neural consumer, made for
//!   games that ship their own DLSS, so it is first there.
//! - The RenoDX DLSS 5 add-on, newest stable build, then 4.70: the build every
//!   reporter had working when 5.2.1 broke four games (#96, #86, #76, #100),
//!   and the last one with Enable Upscaling (#109). Then 4.55, the build the
//!   Feeder's host names as passing where newer ones fault in the driver (#69).
//! - The OptiScaler engine last: a different engine altogether, and the one
//!   that fixed Cyberpunk 2077 (#95).
//!
//! Games with a known answer start further down (`PROFILES`).

use std::fs;

use crate::game::{self, Api, GameStatus, Mode};
use crate::installer::{self, Consumer, Engine};

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
/// newer DLSS 5 add-on crashed at the first evaluate where the older builds ran
/// (#96).
const PROFILES: &[(&str, &[Setup], &str)] = &[
    (
        "cyberpunk2077.exe",
        &[OPTI, SF, DLSS5_NEWEST, DLSS5_STEADY, DLSS5_CLASSIC],
        "Cyberpunk 2077 works on the OptiScaler engine (#95)",
    ),
    (
        "dd2.exe",
        &[DLSS5_STEADY, DLSS5_CLASSIC, SF, DLSS5_NEWEST, OPTI],
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
        let mut l = l.to_vec();
        // A game already on ReShade (ours or the user's) cannot start on
        // OptiScaler, and the list only moves down: OptiScaler goes to the end
        // so it is still reachable (Cyberpunk on ReShade, #95).
        if st.reshade && l.first().is_some_and(|s| s.engine == Engine::Opti) {
            let o = l.remove(0);
            l.push(o);
        }
        return l;
    }
    match (st.mode, st.api) {
        // ReShade cannot reach Vulkan from dxgi.dll; OptiScaler can, when the
        // game has its own DLSS for it to read.
        (Mode::Native, Api::Vulkan) if !st.is32() => vec![OPTI],
        (_, Api::Vulkan) => vec![],
        (Mode::Native, _) if !st.is32() => {
            vec![SF, DLSS5_NEWEST, DLSS5_STEADY, DLSS5_CLASSIC, OPTI]
        }
        // No DLSS of its own, or 32-bit: the Feeder carries it, and the DLSS 5
        // add-on is its consumer.
        _ => vec![DLSS5_NEWEST, DLSS5_STEADY, DLSS5_CLASSIC],
    }
}

/// What this tool has in the game folder now, as a setup. `None` when nothing
/// of ours is there, or when it is something the ladder does not carry
/// (Neural Upstream, the standalone AIO). A DLSS 5 add-on with no recorded
/// build predates the tag marker, from when 4.70 was the default.
pub fn installed(st: &GameStatus) -> Option<Setup> {
    if st.upstream || (st.aio && !st.opti) {
        return None;
    }
    if st.opti {
        return Some(OPTI);
    }
    if !st.reshade {
        return None;
    }
    if st.sf && !st.dlss5_addon && st.consumer_dir().join(game::SF_ADDON_MARKER).is_file() {
        return Some(SF);
    }
    if st.dlss5_addon {
        let tag = fs::read_to_string(st.consumer_dir().join(game::DLSS5_ADDON_MARKER))
            .ok()
            .map(|t| t.trim().to_owned());
        return Some(match tag.as_deref() {
            Some(t) if t == installer::RENODX_CLASSIC_TAG => DLSS5_CLASSIC,
            Some(t) if t == installer::RENODX_STEADY_TAG => DLSS5_STEADY,
            Some(_) => DLSS5_NEWEST,
            None => DLSS5_STEADY,
        });
    }
    None
}

/// Something is in the folder that the ladder must not overwrite: Neural
/// Upstream or the standalone AIO picked by hand, or a setup that is not on
/// this game's ladder (4.55 on a profile that does not list it, say). Those
/// are shown under Advanced and left as they are.
pub fn hand_chosen(st: &GameStatus) -> bool {
    if st.upstream || (st.aio && !st.opti) {
        return true;
    }
    installed(st).is_some_and(|s| !ladder(st).contains(&s))
}

/// The rung the game is on: where what is installed sits on its ladder, or the
/// top when nothing is.
pub fn level(st: &GameStatus) -> usize {
    let l = ladder(st);
    installed(st)
        .and_then(|s| l.iter().position(|x| *x == s))
        .unwrap_or(0)
}

/// For a setup picked by hand, what it is, in words.
pub fn hand_label(st: &GameStatus) -> Option<String> {
    if !hand_chosen(st) {
        return None;
    }
    Some(if st.upstream {
        "ReShade + Neural Upstream".to_owned()
    } else if st.aio && !st.opti {
        "ReShade + standalone AIO".to_owned()
    } else {
        installed(st).map(|s| label(&s)).unwrap_or_default()
    })
}

/// The setup for this game now.
pub fn current(st: &GameStatus) -> Option<Setup> {
    if hand_chosen(st) {
        return None;
    }
    ladder(st).get(level(st)).copied()
}

/// The next rung, when there is one.
pub fn next(st: &GameStatus) -> Option<(usize, Setup)> {
    if hand_chosen(st) {
        return None;
    }
    let n = level(st) + 1;
    ladder(st).get(n).copied().map(|s| (n, s))
}

/// The consumer and add-on build values a setup puts in the environment the
/// install steps read.
pub fn env_values(s: &Setup) -> (&'static str, Option<&'static str>) {
    (
        match s.consumer {
            Consumer::ShortFuse => "sf",
            Consumer::Dlss5 => "dlss5",
        },
        s.addon_tag,
    )
}

/// Hand a setup to the installer through the environment; the engine is
/// returned for the caller. `picked` is the picker's own choice: its build
/// pin may give way to the driver-fault fallback (#69). A build the user
/// chose (Advanced, or a hand-chosen install being refreshed) holds.
pub fn apply(s: &Setup, picked: bool) -> Engine {
    let (consumer, tag) = env_values(s);
    std::env::set_var(installer::CONSUMER_ENV, consumer);
    match tag {
        Some(t) => std::env::set_var(installer::RENODX_TAG_ENV, t),
        None => std::env::remove_var(installer::RENODX_TAG_ENV),
    }
    if picked {
        std::env::set_var(installer::RENODX_TAG_SOFT_ENV, "1");
    } else {
        std::env::remove_var(installer::RENODX_TAG_SOFT_ENV);
    }
    s.engine
}

/// Forget any setup handed over earlier, so the next game does not inherit it.
pub fn clear() {
    std::env::remove_var(installer::CONSUMER_ENV);
    std::env::remove_var(installer::RENODX_TAG_ENV);
    std::env::remove_var(installer::RENODX_TAG_SOFT_ENV);
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

    fn native(dir: &std::path::Path) -> GameStatus {
        let mut st = stub_status(Mode::Native, Api::Dx12);
        st.exe = dir.join("game.exe");
        st
    }

    #[test]
    fn a_fresh_game_with_its_own_dlss_starts_on_shortfuse() {
        let t = tempfile::tempdir().unwrap();
        let st = native(t.path());
        assert_eq!(
            ladder(&st),
            vec![SF, DLSS5_NEWEST, DLSS5_STEADY, DLSS5_CLASSIC, OPTI]
        );
        assert_eq!(current(&st), Some(SF));
        assert_eq!(next(&st), Some((1, DLSS5_NEWEST)));
    }

    /// What is in the folder decides the rung, so Install and Update refresh
    /// the build a user is on (including one they pinned) instead of moving it.
    #[test]
    fn an_existing_install_stays_on_the_setup_it_has() {
        let t = tempfile::tempdir().unwrap();
        let mut st = native(t.path());
        st.reshade = true;
        st.dlss5_addon = true;
        // No tag recorded: from when 4.70 was the default.
        assert_eq!(current(&st), Some(DLSS5_STEADY));
        let m = t.path().join(game::DLSS5_ADDON_MARKER);
        fs::write(&m, installer::RENODX_CLASSIC_TAG).unwrap();
        assert_eq!(current(&st), Some(DLSS5_CLASSIC));
        assert_eq!(next(&st), Some((4, OPTI)));
        fs::write(&m, "renodx-dlss5-6.5.3").unwrap();
        assert_eq!(current(&st), Some(DLSS5_NEWEST));
        st.dlss5_addon = false;
        st.sf = true;
        // A ShortFuse add-on this tool did not place is not our step.
        assert_eq!(current(&st), Some(SF));
        assert_eq!(installed(&st), None);
        fs::write(t.path().join(game::SF_ADDON_MARKER), "renodx-dlss-SF-1").unwrap();
        assert_eq!(installed(&st), Some(SF));
        st.sf = false;
        st.reshade = false;
        st.opti = true;
        assert_eq!(current(&st), Some(OPTI));
        assert_eq!(next(&st), None);
    }

    /// Cyberpunk on ReShade stays on ReShade: the profile's OptiScaler rung is
    /// for a fresh install, not a reason to run OptiScaler over ReShade.
    #[test]
    fn a_profile_does_not_move_an_existing_install() {
        let t = tempfile::tempdir().unwrap();
        let mut st = native(t.path());
        st.exe = t.path().join("Cyberpunk2077.exe");
        assert_eq!(current(&st), Some(OPTI));
        st.reshade = true;
        st.dlss5_addon = true;
        assert_eq!(current(&st), Some(DLSS5_STEADY));
        assert!(reason(&st).contains("#95"));
        st.exe = t.path().join("DD2.exe");
        st.dlss5_addon = false;
        st.reshade = false;
        assert_eq!(current(&st), Some(DLSS5_STEADY));
    }

    /// Cyberpunk on ReShade (the user's own, or ours) starts on ReShade and
    /// still reaches OptiScaler as its last step.
    #[test]
    fn cyberpunk_on_reshade_starts_on_reshade_and_ends_on_optiscaler() {
        let t = tempfile::tempdir().unwrap();
        let mut st = native(t.path());
        st.exe = t.path().join("Cyberpunk2077.exe");
        st.reshade = true;
        assert_eq!(current(&st), Some(SF));
        assert_eq!(ladder(&st).last(), Some(&OPTI));
        st.dlss5_addon = true;
        fs::write(
            t.path().join(game::DLSS5_ADDON_MARKER),
            installer::RENODX_CLASSIC_TAG,
        )
        .unwrap();
        assert_eq!(next(&st).map(|(_, s)| s), Some(OPTI));
    }

    #[test]
    fn a_hand_picked_setup_is_named() {
        let t = tempfile::tempdir().unwrap();
        let mut st = native(t.path());
        assert_eq!(hand_label(&st), None);
        st.upstream = true;
        assert_eq!(
            hand_label(&st).as_deref(),
            Some("ReShade + Neural Upstream")
        );
    }

    #[test]
    fn feeder_32_bit_and_vulkan_get_their_own_ladders() {
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
        assert_eq!(current(&st), None);
    }

    #[test]
    fn neural_upstream_and_aio_are_left_alone() {
        let t = tempfile::tempdir().unwrap();
        let mut st = native(t.path());
        assert!(!hand_chosen(&st));
        st.upstream = true;
        assert!(hand_chosen(&st));
        assert_eq!(current(&st), None);
        assert_eq!(next(&st), None);
        st.upstream = false;
        st.aio = true;
        assert!(hand_chosen(&st));
    }

    #[test]
    fn env_values_name_the_consumer_and_the_build() {
        assert_eq!(env_values(&SF), ("sf", None));
        assert_eq!(
            env_values(&DLSS5_STEADY),
            ("dlss5", Some(installer::RENODX_STEADY_TAG))
        );
        assert_eq!(env_values(&DLSS5_NEWEST), ("dlss5", None));
    }
}
