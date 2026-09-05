//! egui window, "instrument panel" layout: header with mark, path row, component
//! tiles, one Install button, progress, log.

use crate::diagnose;
use crate::game::{self, GameStatus};
use crate::installer::{self, Engine, StepState};
use crate::library::{self, Game, Store};
use crate::logo;
use crate::net;
use crate::renodx;
use crate::text;
use crate::theme::{self as t};
use crate::update;
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke, StrokeKind, Vec2,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

#[derive(Clone)]
enum UpdateState {
    Idle,
    Available(update::Available),
    Downloading(u8, String),
    Restarting,
    Failed(String),
}

enum Msg {
    Progress(u8, String),
    Log(LogLine),
    Finished(Result<String, String>),
}

#[derive(Clone)]
enum LogLine {
    Step(String),
    Ok(String),
    Fail(String),
    Plain(String),
}

pub struct App {
    exe_text: String,
    status: Option<Result<GameStatus, String>>,
    running: bool,
    progress: u8,
    progress_msg: String,
    log: Vec<LogLine>,
    rx: Option<Receiver<Msg>>,
    confirm_remove: bool,
    last_error: Option<String>,
    candidates: Vec<PathBuf>,
    resolved_exe: Option<PathBuf>,
    engine: Engine,
    update: UpdateState,
    update_rx: Option<Receiver<UpdateState>>,
    skipped_version: String,
    /// A card whose install just finished and whose state has to be re-read.
    pending_refresh: Option<usize>,
    /// The card being installed from the Games page, so the progress is shown
    /// where the user started it instead of throwing them onto another page.
    updating: Option<usize>,
    /// One refreshed card, after its install finished.
    meta_one_rx: Option<Receiver<(usize, GameMeta)>>,
    /// The user pressed "Check for updates": the answer is worth showing even
    /// when it is "nothing new", which a background check never says.
    checked_manually: bool,
    /// When the last update check ran. The check used to happen once, at
    /// startup, so a release published while the window was open was never
    /// noticed -- the tool sat there saying it was current for days.
    last_update_check: std::time::Instant,
    /// "Also install the RenoDX HDR mod" checkbox.
    renodx_on: bool,
    renodx: RenodxLookup,
    renodx_rx: Option<Receiver<RenodxLookup>>,
    /// Exe the current lookup belongs to, so a refresh does not re-fetch.
    renodx_for: Option<PathBuf>,
    page: Page,
    games: Vec<Game>,
    scanning: bool,
    scan_rx: Option<Receiver<Vec<Game>>>,
    /// Per game: decoded poster texture, or `None` once decoding failed.
    posters: HashMap<usize, Option<egui::TextureHandle>>,
    poster_rx: Option<Receiver<(usize, Option<egui::ColorImage>)>>,
    meta: HashMap<usize, GameMeta>,
    meta_rx: Option<Receiver<(usize, GameMeta)>>,
    search: String,
    /// Official store marks (Simple Icons, CC0), white on transparent, tinted at paint time.
    store_icons: HashMap<Store, egui::TextureHandle>,
    kofi_icon: Option<egui::TextureHandle>,
}

pub const KOFI_URL: &str = "https://ko-fi.com/kindiboy";
/// Ko-fi's brand red.
const KOFI_RED: Color32 = Color32::from_rgb(0xff, 0x5e, 0x5b);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Games,
    Setup,
    About,
}

/// What a poster card asked for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CardAction {
    #[default]
    None,
    /// Left click: open the game on the Setup page.
    Open,
    /// Right click ▸ Update: install straight away, with the engine the game
    /// already has, and show the Setup page so the progress is visible.
    Update,
    /// Right click ▸ Forget: drop a hand-added game from the list.
    Forget,
}

/// What the tool knows about an installed game without touching it.
#[derive(Debug, Clone)]
struct GameMeta {
    api: &'static str,
    has_dlss: bool,
    addon: bool,
    ready: bool,
    /// This tool installed into that folder: the game is listed in its own
    /// section at the top rather than under its store.
    installed: bool,
    /// Components this tool placed that upstream has since moved past.
    stale: Vec<String>,
}

#[derive(Debug, Clone)]
enum RenodxLookup {
    Idle,
    Pending,
    Found(renodx::Mod),
    NotFound,
    Failed(String),
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        t::install(&cc.egui_ctx);
        logo::set_taskbar_icon(cc);
        let exe_text = cc
            .storage
            .and_then(|s| s.get_string("exe"))
            .unwrap_or_default();
        let mut app = App {
            exe_text,
            status: None,
            running: false,
            progress: 0,
            progress_msg: String::new(),
            log: Vec::new(),
            rx: None,
            confirm_remove: false,
            last_error: None,
            candidates: Vec::new(),
            resolved_exe: None,
            engine: Engine::default(),
            update: UpdateState::Idle,
            update_rx: None,
            renodx_on: false,
            renodx: RenodxLookup::Idle,
            renodx_rx: None,
            renodx_for: None,
            page: Page::Games,
            games: Vec::new(),
            scanning: false,
            scan_rx: None,
            posters: HashMap::new(),
            poster_rx: None,
            meta: HashMap::new(),
            meta_rx: None,
            search: String::new(),
            store_icons: HashMap::new(),
            kofi_icon: None,
            updating: None,
            pending_refresh: None,
            meta_one_rx: None,
            checked_manually: false,
            last_update_check: std::time::Instant::now(),
            skipped_version: cc
                .storage
                .and_then(|s| s.get_string("skip_version"))
                .unwrap_or_default(),
        };
        app.refresh();
        if app.resolved_exe.is_some() {
            app.page = Page::Setup;
        }
        app.start_update_check();
        app.start_scan(&cc.egui_ctx);
        app
    }

    fn exe(&self) -> Option<PathBuf> {
        self.resolved_exe.clone()
    }

    fn input_path(&self) -> PathBuf {
        PathBuf::from(self.exe_text.trim().trim_matches('"'))
    }

    /// Text box accepts an exe or a game folder; a folder is resolved to its game exe.
    fn refresh(&mut self) {
        let input = self.input_path();
        let prev = self.resolved_exe.take();
        self.candidates.clear();
        if input.as_os_str().is_empty() {
            self.status = None;
            return;
        }
        match game::resolve_target(&input) {
            Ok((exe, cands)) => {
                self.resolved_exe = Some(match prev {
                    Some(p) if cands.len() > 1 && cands.contains(&p) => p,
                    _ => exe,
                });
                self.candidates = cands;
            }
            Err(e) => {
                self.status = Some(Err(format!("{e:#}")));
                return;
            }
        }
        self.inspect_resolved();
    }

    fn inspect_resolved(&mut self) {
        self.status = self
            .resolved_exe
            .as_ref()
            .map(|p| game::inspect(p).map_err(|e| format!("{e:#}")));
        if self.resolved_exe != self.renodx_for {
            self.renodx_for = self.resolved_exe.clone();
            self.renodx_on = false;
            self.start_renodx_lookup();
        }
    }

    fn start_renodx_lookup(&mut self) {
        let Some(exe) = self.exe() else {
            self.renodx = RenodxLookup::Idle;
            return;
        };
        let (tx, rx) = channel::<RenodxLookup>();
        self.renodx_rx = Some(rx);
        self.renodx = RenodxLookup::Pending;
        thread::spawn(move || {
            let r = match net::client().and_then(|c| renodx::lookup(&c, &exe)) {
                Ok(Some(m)) => RenodxLookup::Found(m),
                Ok(None) => RenodxLookup::NotFound,
                Err(e) => RenodxLookup::Failed(format!("{e:#}")),
            };
            let _ = tx.send(r);
        });
    }

    fn pump_renodx(&mut self) {
        let Some(rx) = &self.renodx_rx else { return };
        if let Ok(r) = rx.try_recv() {
            self.renodx = r;
            self.renodx_rx = None;
        }
    }

    fn start(&mut self, remove: Option<bool>) {
        let Some(exe) = self.exe() else { return };
        let engine = self.engine;
        let with_renodx = self.renodx_on;
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
        self.rx = Some(rx);
        self.running = true;
        self.progress = 0;
        self.progress_msg.clear();
        self.log.clear();
        self.last_error = None;
        thread::spawn(move || {
            let out = if let Some(everything) = remove {
                let res = if everything {
                    installer::uninstall_all(&exe).map(|(mut r, kept)| {
                        r.extend(kept);
                        r
                    })
                } else {
                    installer::uninstall(&exe)
                };
                res.map(|r| {
                    format!(
                        "Removed: {}",
                        if r.is_empty() {
                            "nothing".into()
                        } else {
                            r.join(", ")
                        }
                    )
                })
                .map_err(|e| format!("{e:#}"))
            } else {
                let p_tx = tx.clone();
                let s_tx = tx.clone();
                installer::run_all_with(
                    &exe,
                    engine,
                    with_renodx,
                    &move |pct, msg| {
                        let _ = p_tx.send(Msg::Progress(pct, msg.to_owned()));
                    },
                    &move |i, n, name, state, detail| {
                        let line = match state {
                            StepState::Start => LogLine::Step(format!("[{}/{n}] {name}", i + 1)),
                            StepState::Done => LogLine::Ok(format!("ok: {detail}")),
                            StepState::Error => LogLine::Fail(format!("FAILED: {detail}")),
                        };
                        let _ = s_tx.send(Msg::Log(line));
                    },
                )
                .map(|_| {
                    if engine == Engine::Opti {
                        "Done. In game: Insert opens the OptiScaler overlay → enable Neural Rendering.".to_owned()
                    } else {
                        "Done. In game: Home opens ReShade → Add-ons tab → DLSS 5 Neural Rendering → enable. (Home tab saying \"no effect files\" is normal on games with their own DLSS.)".to_owned()
                    }
                })
                .map_err(|e| format!("{e:#}"))
            };
            let _ = tx.send(Msg::Finished(out));
        });
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel::<Vec<Game>>();
        self.scan_rx = Some(rx);
        self.scanning = true;
        self.games.clear();
        self.posters.clear();
        self.meta.clear();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(library::scan());
            ctx.request_repaint();
        });
    }

    /// Decode posters and inspect each game in the background, oldest last.
    fn start_game_details(&mut self, ctx: &egui::Context) {
        let games = self.games.clone();
        let (ptx, prx) = channel();
        let (mtx, mrx) = channel();
        self.poster_rx = Some(prx);
        self.meta_rx = Some(mrx);
        let ctx2 = ctx.clone();
        let g2 = games.clone();
        thread::spawn(move || {
            // One lookup for the whole scan; every game is then compared
            // against it by reading the markers this tool wrote.
            let latest = net::client()
                .map(|c| installer::Latest::fetch(&c))
                .unwrap_or_default();
            for (i, g) in g2.iter().enumerate() {
                let meta = game::resolve_target(&g.dir)
                    .and_then(|(exe, _)| game::inspect(&exe))
                    .ok()
                    .map(|st| GameMeta {
                        api: match st.api {
                            game::Api::Vulkan => "Vulkan",
                            game::Api::Dx10 => "DirectX 10",
                            game::Api::Dx11 => "DirectX 11",
                            game::Api::Dx12 => "DirectX 12",
                            game::Api::Unknown => "DirectX 12?",
                        },
                        has_dlss: st.mode == game::Mode::Native,
                        addon: st.dlss5_addon || st.opti,
                        ready: st.complete(),
                        installed: game::installed_by_tool(st.game_dir()),
                        stale: installer::stale_components(st.game_dir(), &latest),
                    });
                if let Some(m) = meta {
                    let _ = mtx.send((i, m));
                }
                if i % 8 == 7 {
                    ctx2.request_repaint();
                }
            }
            ctx2.request_repaint();
        });
        let ctx3 = ctx.clone();
        thread::spawn(move || {
            let client = net::client().ok();
            for (i, g) in games.iter().enumerate() {
                let img = client
                    .as_ref()
                    .and_then(|c| library::poster_rgba(c, &g.poster))
                    .map(|rgba| {
                        // Cards are 150 px wide; 300x450 keeps 2x for HiDPI and saves VRAM.
                        let (w, h) = (rgba.width(), rgba.height());
                        let scale = (300.0 / w as f32).min(450.0 / h as f32).min(1.0);
                        let (nw, nh) = (
                            ((w as f32 * scale) as u32).max(1),
                            ((h as f32 * scale) as u32).max(1),
                        );
                        let small = image::imageops::resize(
                            &rgba,
                            nw,
                            nh,
                            image::imageops::FilterType::Triangle,
                        );
                        egui::ColorImage::from_rgba_unmultiplied(
                            [nw as usize, nh as usize],
                            small.as_raw(),
                        )
                    });
                let _ = ptx.send((i, img));
                if i % 4 == 3 {
                    ctx3.request_repaint();
                }
            }
            ctx3.request_repaint();
        });
    }

    fn pump_library(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.scan_rx {
            if let Ok(games) = rx.try_recv() {
                self.games = games;
                self.scanning = false;
                self.scan_rx = None;
                self.start_game_details(ctx);
            }
        }
        if let Some(rx) = &self.poster_rx {
            let mut got = Vec::new();
            while let Ok(m) = rx.try_recv() {
                got.push(m);
            }
            for (i, img) in got {
                let tex = img.map(|im| {
                    ctx.load_texture(format!("poster{i}"), im, egui::TextureOptions::LINEAR)
                });
                self.posters.insert(i, tex);
            }
        }
        if let Some(rx) = &self.meta_rx {
            while let Ok((i, m)) = rx.try_recv() {
                self.meta.insert(i, m);
            }
        }
    }

    /// Add a game by hand and keep it: it comes back on the next start (#28).
    fn add_game(&mut self, path: PathBuf, ctx: &egui::Context) {
        library::remember_added(&path);
        let mut added = library::added_games(std::slice::from_ref(&path));
        if let Some(g) = added.pop() {
            self.games.insert(0, g);
            library::sort_and_dedupe(&mut self.games);
            self.posters.clear();
            self.meta.clear();
            self.start_game_details(ctx);
        }
        self.open_game(path);
    }

    /// Right click ▸ Update: install with the engine the game already carries,
    /// without leaving the Games page -- the card itself shows the progress.
    fn update_game(&mut self, path: PathBuf, index: usize) {
        self.exe_text = path.to_string_lossy().into_owned();
        self.refresh();
        if let Some(Ok(st)) = &self.status {
            // Whatever stops the Install button stops this too (anti-cheat, a
            // foreign dxgi.dll): the Setup page is now open and says why.
            if !st.problems.is_empty() {
                return;
            }
            self.engine = if st.opti {
                Engine::Opti
            } else {
                Engine::ReShade
            };
            // A RenoDX mod that is already installed stays installed: the
            // step is only added when the lookup has found one, and Install
            // refreshes what is there either way.
            self.updating = Some(index);
            self.start(None);
        } else {
            // Could not even inspect it: the Setup page is the only place
            // that can explain why.
            self.page = Page::Setup;
        }
    }

    /// Re-read one game after its install, so the card stops claiming an
    /// update is available the moment it is not.
    fn refresh_one(&mut self, index: usize, ctx: &egui::Context) {
        let Some(g) = self.games.get(index).cloned() else {
            return;
        };
        let (tx, rx) = channel();
        self.meta_one_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let latest = net::client()
                .map(|c| installer::Latest::fetch(&c))
                .unwrap_or_default();
            if let Some(m) = game::resolve_target(&g.dir)
                .and_then(|(exe, _)| game::inspect(&exe))
                .ok()
                .map(|st| GameMeta {
                    api: match st.api {
                        game::Api::Vulkan => "Vulkan",
                        game::Api::Dx10 => "DirectX 10",
                        game::Api::Dx11 => "DirectX 11",
                        game::Api::Dx12 => "DirectX 12",
                        game::Api::Unknown => "DirectX 12?",
                    },
                    has_dlss: st.mode == game::Mode::Native,
                    addon: st.dlss5_addon || st.opti,
                    ready: st.complete(),
                    installed: game::installed_by_tool(st.game_dir()),
                    stale: installer::stale_components(st.game_dir(), &latest),
                })
            {
                let _ = tx.send((index, m));
            }
            ctx.request_repaint();
        });
    }

    fn open_game(&mut self, path: PathBuf) {
        self.exe_text = path.to_string_lossy().into_owned();
        self.refresh();
        self.page = Page::Setup;
    }

    /// Re-check at most this often while the window is open.
    const UPDATE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

    /// Called every frame: cheap, and only starts a request when the interval
    /// has passed and nothing else is in flight.
    fn maybe_recheck_update(&mut self) {
        if self.update_rx.is_some() || !matches!(self.update, UpdateState::Idle) {
            return;
        }
        if self.last_update_check.elapsed() >= Self::UPDATE_CHECK_EVERY {
            self.start_update_check();
        }
    }

    fn start_update_check(&mut self) {
        self.last_update_check = std::time::Instant::now();
        let (tx, rx) = channel::<UpdateState>();
        self.update_rx = Some(rx);
        thread::spawn(move || {
            let st = match update::check() {
                Ok(Some(av)) => UpdateState::Available(av),
                _ => UpdateState::Idle,
            };
            let _ = tx.send(st);
        });
    }

    fn start_update_download(&mut self, av: update::Available) {
        let (tx, rx) = channel::<UpdateState>();
        self.update_rx = Some(rx);
        self.update = UpdateState::Downloading(0, "Starting".into());
        thread::spawn(move || {
            let p_tx = tx.clone();
            let res = update::download_and_swap(&av, &move |pct, msg| {
                let _ = p_tx.send(UpdateState::Downloading(pct, msg.to_owned()));
            });
            match res {
                Ok(exe) => {
                    let _ = tx.send(UpdateState::Restarting);
                    if update::relaunch(&exe).is_ok() {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        std::process::exit(0);
                    }
                }
                Err(e) => {
                    let _ = tx.send(UpdateState::Failed(format!("{e:#}")));
                }
            }
        });
    }

    fn run_diagnose(&mut self) {
        let Some(exe) = self.exe() else { return };
        self.log.clear();
        self.progress_msg.clear();
        match diagnose::run(&exe) {
            Ok(findings) => {
                for f in findings {
                    let line = match f.level {
                        diagnose::Level::Ok => LogLine::Ok(format!("ok: {}", text::tidy(&f.text))),
                        diagnose::Level::Warn => {
                            LogLine::Plain(format!("warn: {}", text::tidy(&f.text)))
                        }
                        diagnose::Level::Bad => {
                            LogLine::Fail(format!("FAIL: {}", text::tidy(&f.text)))
                        }
                    };
                    self.log.push(line);
                }
            }
            Err(e) => self.log.push(LogLine::Fail(format!("{e:#}"))),
        }
    }

    fn pump_update(&mut self) {
        let Some(rx) = &self.update_rx else { return };
        let mut last = None;
        while let Ok(m) = rx.try_recv() {
            last = Some(m);
        }
        if let Some(m) = last {
            let skip =
                matches!(&m, UpdateState::Available(av) if av.version == self.skipped_version);
            self.update = if skip { UpdateState::Idle } else { m };
        }
    }

    fn pump(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Progress(p, s) => {
                    self.progress = p;
                    self.progress_msg = s;
                }
                Msg::Log(l) => self.log.push(l),
                Msg::Finished(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            self.rx = None;
            self.running = false;
            if let Some(i) = self.updating.take() {
                self.pending_refresh = Some(i);
            }
            match r {
                Ok(msg) => {
                    self.progress = 100;
                    self.progress_msg = "Done.".into();
                    self.log.push(LogLine::Plain(msg));
                }
                Err(e) => {
                    self.progress_msg = "Failed.".into();
                    self.log.push(LogLine::Fail(e.clone()));
                    self.last_error = Some(e);
                }
            }
            self.refresh();
        }
    }
}

struct Tile {
    title: &'static str,
    detail: &'static str,
    ok: fn(&GameStatus) -> bool,
    optional: bool,
}

const TILES_FEEDER: [Tile; 6] = [
    Tile {
        title: "ReShade",
        detail: "add-on build · dxgi.dll",
        ok: |s| s.reshade,
        optional: false,
    },
    Tile {
        title: "Shader headers",
        detail: "ReShade.fxh · ReShadeUI.fxh · DrawText.fxh",
        ok: |s| s.headers,
        optional: false,
    },
    Tile {
        title: "DLSS5-Feeder",
        detail: "dlss5-feed.addon64 · DLSS5_Feed.fx",
        ok: |s| s.feeder,
        optional: false,
    },
    Tile {
        title: "LumeniteFX",
        detail: "motion vectors · Kernel 2.0",
        ok: |s| s.lumenite,
        optional: false,
    },
    Tile {
        title: "DLSS 5 add-on · leaked",
        detail: "renodx-dlss5.addon64 · nvngx_dlssnr.dll",
        ok: |s| s.dlss5_addon && s.dlssnr,
        optional: false,
    },
    Tile {
        title: "nvngx_dlss.dll",
        detail: "DLSS runtime · the Feeder's NGX session needs it",
        ok: |s| s.dlss,
        optional: false,
    },
];

const TILE_OPTI: Tile = Tile {
    title: "OptiScaler + NR pass",
    detail: "Dagherbou fork as dxgi.dll · Insert opens its overlay",
    ok: |s| s.opti,
    optional: false,
};

const TILES_NATIVE: [Tile; 4] = [
    Tile {
        title: "Game DLSS",
        detail: "nvngx_dlss.dll shipped by the game · add-on hooks it directly",
        ok: |_| true,
        optional: false,
    },
    Tile {
        title: "ReShade",
        detail: "add-on build · dxgi.dll",
        ok: |s| s.reshade,
        optional: false,
    },
    Tile {
        title: "DLSS 5 add-on · leaked",
        detail: "renodx-dlss5.addon64 · nvngx_dlssnr.dll",
        ok: |s| s.dlss5_addon && s.dlssnr,
        optional: false,
    },
    Tile {
        title: "DX11 bridge",
        detail: "dlss5-bridge.addon64 (NIGos) · D3D11 games",
        ok: |s| s.bridge,
        optional: true,
    },
];

const TILE_HOST: Tile = Tile {
    title: "host64 helper (32-bit game)",
    detail: "dlss5-feed-host64.exe + 64-bit ReShade · add-on and models live in host64\\",
    ok: |s| s.host_exe && s.host_reshade,
    optional: false,
};

const TILE_RENODX: Tile = Tile {
    title: "RenoDX HDR mod",
    detail: "game-specific renodx-*.addon64 · Home → Add-ons → RenoDX",
    ok: |s| s.renodx_mod.is_some(),
    optional: true,
};

const TILE_REFRAMEWORK: Tile = Tile {
    title: "REFramework",
    detail: "dinput8.dll · RE Engine games need it before ReShade",
    ok: |s| s.reframework,
    optional: false,
};

fn tiles_for(st: Option<&GameStatus>, engine: Engine, renodx_on: bool) -> Vec<&'static Tile> {
    let mut v = base_tiles(st, engine);
    if st.is_some_and(|s| s.re_engine) {
        v.insert(0, &TILE_REFRAMEWORK);
    }
    if st.is_some_and(|s| s.is32()) {
        v.insert(1, &TILE_HOST);
    }
    if renodx_on || st.is_some_and(|s| s.renodx_mod.is_some()) {
        v.push(&TILE_RENODX);
    }
    v
}

fn base_tiles(st: Option<&GameStatus>, engine: Engine) -> Vec<&'static Tile> {
    match st.map(|s| s.mode) {
        Some(game::Mode::Native) if engine == Engine::Opti || st.is_some_and(|s| s.opti) => {
            vec![&TILES_NATIVE[0], &TILE_OPTI, &TILES_NATIVE[2]]
        }
        Some(game::Mode::Native) => {
            let needs_bridge = st.is_some_and(|s| s.needs_bridge());
            TILES_NATIVE
                .iter()
                .filter(|t| t.title != "DX11 bridge" || needs_bridge)
                .collect()
        }
        _ => TILES_FEEDER.iter().collect(),
    }
}

/// One selectable engine card: painted like a component tile, but clickable,
/// with a radio dot, an accent border when chosen and a dimmed body when the
/// game cannot use it.
#[allow(clippy::too_many_arguments)]
fn engine_card(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: bool,
    enabled: bool,
    title: &str,
    lines: &[&str],
    note: &str,
) -> bool {
    let resp = ui.allocate_rect(rect, egui::Sense::click());
    let hovered = enabled && resp.hovered();
    let fill = if selected {
        Color32::from_rgb(0x22, 0x27, 0x30)
    } else if hovered {
        Color32::from_rgb(0x1d, 0x21, 0x29)
    } else {
        t::TILE
    };
    let border = if selected {
        t::ACCENT
    } else if enabled {
        t::BORDER_STRONG
    } else {
        t::BORDER
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(if selected { 2.0 } else { 1.0 }, border),
        StrokeKind::Inside,
    );

    // radio dot
    let c = egui::pos2(rect.left() + 20.0, rect.top() + 22.0);
    if selected {
        painter.circle_filled(c, 8.0, t::ACCENT);
        painter.circle_filled(c, 3.4, t::BG);
    } else {
        painter.circle_stroke(
            c,
            7.5,
            Stroke::new(1.6, if enabled { t::TEXT_DIM } else { t::RING_OFF }),
        );
    }

    // Right-hand pill: what clicking does. Makes the card read as a choice,
    // unlike the flat component list below it.
    let pill_text = if selected {
        "SELECTED"
    } else if enabled {
        "CHOOSE"
    } else {
        "UNAVAILABLE"
    };
    let pill_font = t::plex_semibold(10.0);
    let galley = painter.layout_no_wrap(pill_text.to_owned(), pill_font, t::BG);
    let pill = egui::Rect::from_min_size(
        egui::pos2(rect.right() - galley.size().x - 32.0, rect.top() + 12.0),
        galley.size() + Vec2::new(20.0, 8.0),
    );
    if selected {
        painter.rect_filled(pill, CornerRadius::same(12), t::ACCENT);
        painter.galley(pill.min + Vec2::new(10.0, 4.0), galley, t::BG);
    } else {
        painter.rect_stroke(
            pill,
            CornerRadius::same(12),
            Stroke::new(
                1.0,
                if enabled {
                    t::TEXT_MUTED
                } else {
                    t::BORDER_STRONG
                },
            ),
            StrokeKind::Inside,
        );
        painter.galley(
            pill.min + Vec2::new(10.0, 4.0),
            galley,
            if enabled { t::TEXT_OFF } else { t::TEXT_DIM },
        );
    }

    let title_color = if !enabled {
        t::TEXT_DIM
    } else if selected {
        t::TEXT
    } else {
        t::TEXT_SOFT
    };
    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 38.0, rect.top() + 10.0),
        egui::pos2(pill.left() - 12.0, rect.bottom() - 8.0),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .font(t::plex_semibold(13.5))
                    .color(title_color),
            );
            for l in lines {
                ui.label(RichText::new(*l).font(t::plex(11.0)).color(if enabled {
                    t::TEXT_MUTED
                } else {
                    t::TEXT_DIM
                }));
            }
            if !note.is_empty() {
                ui.label(RichText::new(note).font(t::plex(11.0)).color(t::TEXT_DIM));
            }
        },
    );
    enabled && resp.clicked()
}

fn chip(ui: &mut egui::Ui, text: &str, color: Color32, outlined: bool) {
    Frame::new()
        .stroke(Stroke::new(1.0, if outlined { color } else { t::BORDER }))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(t::mono(10.0)).color(color));
        });
}

fn tile(ui: &mut egui::Ui, rect: egui::Rect, tl: &Tile, st: Option<&GameStatus>) {
    // A status row, not a control: no border, no fill, a check / ring / dash
    // glyph and two lines of text. The hover shows nothing clickable.
    let ok = st.map(tl.ok).unwrap_or(false);
    let dashed = tl.optional && !ok;
    let inner = rect.shrink2(Vec2::new(6.0, 4.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            paint_status_glyph(ui, ok, tl.optional);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let title_color = if ok {
                    t::TEXT
                } else if dashed {
                    t::TEXT_DIM
                } else {
                    t::TEXT_OFF
                };
                ui.label(
                    RichText::new(tl.title)
                        .font(t::plex_medium(12.5))
                        .color(title_color),
                );
                ui.label(
                    RichText::new(tl.detail)
                        .font(t::plex(10.5))
                        .color(if dashed { t::TEXT_DIM } else { t::TEXT_MUTED }),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = if ok {
                    ("installed", t::ACCENT)
                } else if dashed {
                    ("not needed", t::TEXT_DIM)
                } else {
                    ("missing", t::TEXT_MUTED)
                };
                ui.label(RichText::new(text).font(t::plex(10.5)).color(color));
            });
        },
    );
    // Hairline under each row, so the list reads as a table.
    ui.painter().hline(
        rect.left() + 6.0..=rect.right() - 6.0,
        rect.bottom(),
        Stroke::new(1.0, t::BORDER),
    );
}

/// Filled check (installed), hollow ring (missing), dash (optional, not needed).
fn paint_status_glyph(ui: &mut egui::Ui, ok: bool, optional: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    if ok {
        p.circle_filled(c, 7.0, t::ACCENT);
        let s = Stroke::new(1.7, t::BG);
        p.line_segment([c + Vec2::new(-3.2, 0.2), c + Vec2::new(-1.1, 2.3)], s);
        p.line_segment([c + Vec2::new(-1.1, 2.3), c + Vec2::new(3.2, -2.1)], s);
    } else if optional {
        p.line_segment(
            [c + Vec2::new(-4.0, 0.0), c + Vec2::new(4.0, 0.0)],
            Stroke::new(1.6, t::RING_OFF),
        );
    } else {
        p.circle_stroke(c, 6.2, Stroke::new(1.4, t::TEXT_DIM));
    }
}

const CARD_W: f32 = 150.0;
const CARD_GAP: f32 = 14.0;
const CAPTION_H: f32 = 40.0;

impl App {
    fn games_page(&mut self, ui: &mut egui::Ui) {
        if self.store_icons.is_empty() {
            self.store_icons = load_store_icons(ui.ctx());
        }
        // ── header ────────────────────────────────────────────────
        let dx12 = self
            .meta
            .values()
            .filter(|m| m.api.starts_with("DirectX 12"))
            .count();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(RichText::new("Games").font(t::sora(16.0)).color(t::TEXT));
            let summary = if self.scanning {
                "scanning Steam, Epic, GOG and Xbox…".to_owned()
            } else {
                format!("{} found · {dx12} on DirectX 12", self.games.len())
            };
            ui.label(
                RichText::new(summary)
                    .font(t::plex(12.0))
                    .color(t::TEXT_MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let btn = |text: &str, accent: bool| {
                    egui::Button::new(
                        RichText::new(text)
                            .font(t::plex_medium(12.5))
                            .color(if accent { t::BG } else { t::TEXT_OFF }),
                    )
                    .fill(if accent {
                        t::ACCENT
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if accent { t::ACCENT } else { t::BORDER_STRONG },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(96.0, 34.0))
                };
                if ui
                    // Rescanning mid-install would renumber the cards under the
                    // one being worked on.
                    .add_enabled(!self.scanning && !self.running, btn("Rescan", true))
                    .clicked()
                {
                    self.start_scan(ui.ctx());
                }
                if ui.add(btn("Add a folder", false)).clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title("Pick the game's install folder")
                        .pick_folder()
                    {
                        self.add_game(p, ui.ctx());
                    }
                }
                if ui.add(btn("Add a game", false)).clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Executables", &["exe"])
                        .pick_file()
                    {
                        self.add_game(p, ui.ctx());
                    }
                }
                let search = egui::TextEdit::singleline(&mut self.search)
                    .font(t::plex(12.0))
                    .hint_text(RichText::new("Search").color(t::TEXT_DIM))
                    .desired_width(160.0);
                ui.add(search);
            });
        });
        ui.add_space(4.0);

        // ── grid, grouped by store ────────────────────────────────
        let needle = self.search.trim().to_ascii_lowercase();
        let mut clicked: Option<(PathBuf, usize)> = None;
        let mut forgotten: Option<PathBuf> = None;
        let mut update = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                // Room for the scrollbar, then as many columns as fit at the
                // minimum width; the cards then grow to fill the row (up to 190 px).
                let avail = ui.available_width() - 14.0;
                let cols = ((avail + CARD_GAP) / (CARD_W + CARD_GAP)).floor().max(1.0) as usize;
                let card_w =
                    ((avail - CARD_GAP * (cols as f32 - 1.0)) / cols as f32).clamp(CARD_W, 190.0);
                let poster_h = (card_w * 1.5).round();
                // None = "Installed by this tool", always first: the whole
                // point is to see at a glance what has been modified and
                // what has fallen behind.
                let sections: [Option<Store>; 6] = [
                    None,
                    Some(Store::Manual),
                    Some(Store::Steam),
                    Some(Store::Epic),
                    Some(Store::Gog),
                    Some(Store::Xbox),
                ];
                for section in sections {
                    let idx: Vec<usize> = self
                        .games
                        .iter()
                        .enumerate()
                        .filter(|(i, g)| {
                            let ours = self.meta.get(i).is_some_and(|m| m.installed);
                            let in_section = match section {
                                None => ours,
                                Some(st) => g.store == st && !ours,
                            };
                            in_section
                                && (needle.is_empty()
                                    || g.title.to_ascii_lowercase().contains(&needle))
                        })
                        .map(|(i, _)| i)
                        .collect();
                    if idx.is_empty() {
                        continue;
                    }
                    let ready = idx
                        .iter()
                        .filter(|i| self.meta.get(i).is_some_and(|m| m.ready))
                        .count();
                    let stale = idx
                        .iter()
                        .filter(|i| self.meta.get(i).is_some_and(|m| !m.stale.is_empty()))
                        .count();
                    ui.horizontal(|ui| {
                        ui.set_max_width(avail);
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new(match section {
                                None => "Installed by this tool",
                                Some(st) => st.label(),
                            })
                            .font(t::plex_semibold(13.0))
                            .color(t::TEXT),
                        );
                        ui.label(
                            RichText::new(idx.len().to_string())
                                .font(t::plex(12.0))
                                .color(t::TEXT_DIM),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if stale > 0 {
                                ui.label(
                                    RichText::new(format!("{stale} need updating"))
                                        .font(t::plex(11.5))
                                        .color(t::WARN),
                                );
                            } else if ready > 0 {
                                ui.label(
                                    RichText::new(format!("{ready} ready for DLSS 5"))
                                        .font(t::plex(11.5))
                                        .color(t::ACCENT),
                                );
                            }
                        });
                    });
                    for row in idx.chunks(cols) {
                        let (row_rect, _) = ui.allocate_exact_size(
                            Vec2::new(avail, poster_h + CAPTION_H),
                            egui::Sense::hover(),
                        );
                        for (k, &i) in row.iter().enumerate() {
                            let x = row_rect.left() + k as f32 * (card_w + CARD_GAP);
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(x, row_rect.top()),
                                Vec2::new(card_w, poster_h + CAPTION_H),
                            );
                            let action = self.game_card(ui, rect, i);
                            if action == CardAction::Forget {
                                forgotten = Some(self.games[i].dir.clone());
                            }
                            if matches!(action, CardAction::Open | CardAction::Update) {
                                update = action == CardAction::Update;
                                let g = &self.games[i];
                                // The folder, so the exe finder ranks every candidate:
                                // a store's launch exe can be a bootstrapper (Epic names
                                // Satisfactory's FactoryGameEGS.exe, the real one is the
                                // -Shipping.exe under Engine\Binaries\Win64, #29). The
                                // store's exe is the fallback when nothing is found.
                                let path = match game::resolve_target(&g.dir) {
                                    Ok(_) => g.dir.clone(),
                                    Err(_) => match &g.exe_hint {
                                        Some(e) if e.is_file() && game::exe_bitness(e).is_ok() => {
                                            e.clone()
                                        }
                                        _ => g.dir.clone(),
                                    },
                                };
                                clicked = Some((path, i));
                            }
                        }
                    }
                    ui.add_space(10.0);
                }
                if !self.scanning && self.games.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(
                                "No installed games found from Steam, Epic, GOG or Xbox.",
                            )
                            .font(t::plex(13.0))
                            .color(t::TEXT_MUTED),
                        );
                        ui.label(
                            RichText::new("Use Add a folder to point at a game by hand.")
                                .font(t::plex(12.0))
                                .color(t::TEXT_DIM),
                        );
                    });
                }
            });
        if let Some(p) = forgotten {
            library::forget_added(&p);
            self.games
                .retain(|g| !(g.store == Store::Manual && g.dir == p));
        }
        if let Some((p, index)) = clicked {
            if update {
                self.update_game(p, index);
            } else {
                self.open_game(p);
            }
        }
    }

    /// One poster card, and what it was asked to do.
    fn game_card(&self, ui: &mut egui::Ui, rect: egui::Rect, i: usize) -> CardAction {
        let g = &self.games[i];
        let resp = ui.interact(rect, ui.id().with(("card", i)), egui::Sense::click());
        let hovered = resp.hovered();
        let p = ui.painter();
        let r = CornerRadius::same(10);
        p.rect_filled(rect, r, t::TILE);
        let poster =
            egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), rect.height() - CAPTION_H));
        match self.posters.get(&i) {
            Some(Some(tex)) => {
                let top = CornerRadius {
                    nw: 10,
                    ne: 10,
                    sw: 0,
                    se: 0,
                };
                let [tw, th] = tex.size();
                let aspect = tw as f32 / th.max(1) as f32;
                if (aspect - 2.0 / 3.0).abs() < 0.08 {
                    // Real 600x900 art: fill the frame.
                    egui::Image::from_texture(tex)
                        .fit_to_exact_size(poster.size())
                        .corner_radius(top)
                        .paint_at(ui, poster);
                } else {
                    // Landscape header, logo or icon: contain it on the dark card.
                    p.rect_filled(poster, top, t::BG);
                    let scale = (poster.width() / tw as f32).min(poster.height() / th as f32);
                    let size = Vec2::new(tw as f32 * scale, th as f32 * scale);
                    let inner = egui::Rect::from_center_size(poster.center(), size);
                    egui::Image::from_texture(tex)
                        .fit_to_exact_size(size)
                        .paint_at(ui, inner);
                }
            }
            Some(None) | None => {
                // No artwork (yet): title on the dark card.
                p.rect_filled(
                    poster,
                    CornerRadius {
                        nw: 10,
                        ne: 10,
                        sw: 0,
                        se: 0,
                    },
                    t::BG,
                );
                p.text(
                    poster.center(),
                    egui::Align2::CENTER_CENTER,
                    if self.posters.contains_key(&i) {
                        &g.title
                    } else {
                        "…"
                    },
                    t::sora(13.0),
                    t::TEXT_MUTED,
                );
            }
        }
        let p = ui.painter();
        // DirectX chip, top right.
        if let Some(m) = self.meta.get(&i) {
            let font = t::plex_semibold(9.5);
            let galley = p.layout_no_wrap(m.api.to_owned(), font.clone(), t::BG);
            let pad = Vec2::new(7.0, 3.0);
            let chip = egui::Rect::from_min_size(
                egui::pos2(
                    poster.right() - galley.size().x - pad.x * 2.0 - 8.0,
                    poster.top() + 8.0,
                ),
                galley.size() + pad * 2.0,
            );
            p.rect_filled(chip, CornerRadius::same(6), t::ACCENT);
            p.galley(chip.min + pad, galley, t::BG);
            // A game this tool set up whose files upstream has moved past.
            if !m.stale.is_empty() && self.updating != Some(i) {
                let galley = p.layout_no_wrap("UPDATE".to_owned(), font, t::BG);
                let badge = egui::Rect::from_min_size(
                    egui::pos2(poster.left() + 8.0, poster.top() + 8.0),
                    galley.size() + pad * 2.0,
                );
                p.rect_filled(badge, CornerRadius::same(6), t::WARN);
                p.galley(badge.min + pad, galley, t::BG);
            }
            // Status line over the bottom of the poster.
            let band = egui::Rect::from_min_max(
                egui::pos2(poster.left(), poster.bottom() - 26.0),
                poster.max,
            );
            p.rect_filled(band, CornerRadius::ZERO, Color32::from_black_alpha(170));
            let mut x = band.left() + 10.0;
            for (on, label) in [
                (m.has_dlss, if m.has_dlss { "DLSS" } else { "no DLSS" }),
                (m.addon, if m.addon { "add-on" } else { "no add-on" }),
            ] {
                let cy = band.center().y;
                p.circle_filled(
                    egui::pos2(x + 3.0, cy),
                    3.0,
                    if on { t::ACCENT } else { t::TEXT_DIM },
                );
                let galley = p.layout_no_wrap(label.to_owned(), t::plex_medium(10.5), t::TEXT_SOFT);
                p.galley(
                    egui::pos2(x + 11.0, cy - galley.size().y / 2.0),
                    galley.clone(),
                    t::TEXT_SOFT,
                );
                x += 11.0 + galley.size().x + 12.0;
            }
        }
        // Caption: store mark on the left, title beside it.
        let cap = egui::Rect::from_min_max(egui::pos2(rect.left(), poster.bottom()), rect.max);
        let mark = egui::Rect::from_min_size(
            egui::pos2(cap.left() + 10.0, cap.top() + 9.0),
            Vec2::splat(16.0),
        );
        store_mark(ui, &self.store_icons, mark, g.store, t::TEXT_OFF);
        let title_x = mark.right() + 7.0;
        let title = p.layout(
            g.title.clone(),
            t::plex_medium(12.0),
            t::TEXT,
            cap.right() - title_x - 8.0,
        );
        let clip = egui::Rect::from_min_max(cap.min, egui::pos2(cap.right(), cap.bottom() - 4.0));
        ui.painter().with_clip_rect(clip).galley(
            egui::pos2(title_x, cap.top() + 8.0),
            title,
            t::TEXT,
        );
        p.rect_stroke(
            rect,
            r,
            Stroke::new(1.0, if hovered { t::ACCENT } else { t::BORDER }),
            StrokeKind::Inside,
        );
        if hovered {
            let stale = self
                .meta
                .get(&i)
                .filter(|m| !m.stale.is_empty())
                .map(|m| format!("\n\nOut of date:\n  {}", m.stale.join("\n  ")))
                .unwrap_or_default();
            resp.clone()
                .on_hover_text(format!("{}\n{}{stale}", g.title, g.dir.display()));
        }
        // Being installed right now: dim the poster, say so, and show how far
        // along it is, right where the user asked for it.
        if self.updating == Some(i) {
            let p = ui.painter();
            p.rect_filled(poster, r, Color32::from_black_alpha(190));
            let pct = self.progress.min(100);
            let title = p.layout_no_wrap(
                if self.running {
                    format!("Updating… {pct}%")
                } else {
                    "Finishing…".to_owned()
                },
                t::plex_semibold(13.0),
                t::TEXT,
            );
            p.galley(
                egui::pos2(
                    poster.center().x - title.size().x / 2.0,
                    poster.center().y - 22.0,
                ),
                title,
                t::TEXT,
            );
            // Which step, so a long download does not look stuck.
            if !self.progress_msg.is_empty() {
                let step = p.layout(
                    self.progress_msg.clone(),
                    t::plex(10.5),
                    t::TEXT_SOFT,
                    poster.width() - 20.0,
                );
                p.galley(
                    egui::pos2(poster.center().x - step.size().x / 2.0, poster.center().y),
                    step,
                    t::TEXT_SOFT,
                );
            }
            let track = egui::Rect::from_min_size(
                egui::pos2(poster.left() + 16.0, poster.bottom() - 26.0),
                Vec2::new(poster.width() - 32.0, 4.0),
            );
            p.rect_filled(track, CornerRadius::same(2), t::BORDER_STRONG);
            let done = egui::Rect::from_min_size(
                track.min,
                Vec2::new(track.width() * pct as f32 / 100.0, track.height()),
            );
            p.rect_filled(done, CornerRadius::same(2), t::ACCENT);
        }
        let mut action = if resp.clicked() && self.updating != Some(i) {
            CardAction::Open
        } else {
            CardAction::None
        };
        let installed = self.meta.get(&i).is_some_and(|m| m.installed);
        let stale = self.meta.get(&i).is_some_and(|m| !m.stale.is_empty());
        if installed || g.store == Store::Manual {
            resp.context_menu(|ui| {
                if installed {
                    let label = if stale { "Update" } else { "Re-install" };
                    if ui
                        .add_enabled(!self.running, egui::Button::new(label))
                        .clicked()
                    {
                        action = CardAction::Update;
                        ui.close();
                    }
                }
                if g.store == Store::Manual && ui.button("Forget this game").clicked() {
                    action = CardAction::Forget;
                    ui.close();
                }
            });
        }
        action
    }
}

/// The stores' own marks (Simple Icons, CC0 1.0), white PNGs tinted at paint time.
const STORE_ICON_PNG: [(Store, &[u8]); 4] = [
    (Store::Steam, include_bytes!("../assets/store-steam.png")),
    (Store::Xbox, include_bytes!("../assets/store-xbox.png")),
    (Store::Epic, include_bytes!("../assets/store-epic.png")),
    (Store::Gog, include_bytes!("../assets/store-gog.png")),
];

fn load_store_icons(ctx: &egui::Context) -> HashMap<Store, egui::TextureHandle> {
    STORE_ICON_PNG
        .iter()
        .filter_map(|(store, png)| {
            let img = image::load_from_memory(png).ok()?.to_rgba8();
            let (w, h) = img.dimensions();
            let ci =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
            Some((
                *store,
                ctx.load_texture(
                    format!("store-{}", store.label()),
                    ci,
                    egui::TextureOptions::LINEAR,
                ),
            ))
        })
        .collect()
}

fn store_mark(
    ui: &egui::Ui,
    icons: &HashMap<Store, egui::TextureHandle>,
    r: egui::Rect,
    store: Store,
    color: Color32,
) {
    if let Some(tex) = icons.get(&store) {
        egui::Image::from_texture(tex)
            .fit_to_exact_size(r.size())
            .tint(color)
            .paint_at(ui, r);
    }
}

fn about_page(ui: &mut egui::Ui) {
    ui.label(RichText::new("About").font(t::sora(16.0)).color(t::TEXT));
    ui.label(
        RichText::new(concat!(
            "DLSS5oneclick v",
            env!("CARGO_PKG_VERSION"),
            " — one click for the leaked DLSS 5 neural-rendering build in any DX11/DX12 game."
        ))
        .font(t::plex(12.5))
        .color(t::TEXT_SOFT),
    );
    ui.add_space(6.0);
    ui.label(RichText::new("Everything it installs is downloaded from the projects that made it. Credits and sources:").font(t::plex(12.0)).color(t::TEXT_MUTED));
    for (name, url) in [
        ("crosire — ReShade", "https://reshade.me"),
        (
            "clshortfuse and the RenoDX community",
            "https://github.com/clshortfuse/renodx",
        ),
        (
            "jlrouzies-fr — DLSS5-Feeder",
            "https://github.com/jlrouzies-fr/DLSS5-Feeder",
        ),
        (
            "umar-afzaal — LumeniteFX",
            "https://github.com/umar-afzaal/LumeniteFX",
        ),
        (
            "RankFTW — RHI and rhi-repo",
            "https://github.com/RankFTW/RHI",
        ),
        (
            "NIGos — dlss5-bridge",
            "https://github.com/NIGos/dlss5-bridge",
        ),
        (
            "Dagherbou — OptiScaler_DLSSNR",
            "https://github.com/Dagherbou/OptiScaler_DLSSNR",
        ),
        (
            "praydog — REFramework",
            "https://github.com/praydog/REFramework",
        ),
        (
            "Source, issues and releases",
            "https://github.com/faisalkindi/DLSS5oneclick",
        ),
    ] {
        ui.hyperlink_to(
            RichText::new(name).font(t::plex(12.0)).color(t::TEXT_OFF),
            url,
        );
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("exe", self.exe_text.clone());
        storage.set_string("skip_version", self.skipped_version.clone());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        self.pump_renodx();
        if let Some(i) = self.pending_refresh.take() {
            self.refresh_one(i, ui.ctx());
        }
        if let Some(rx) = &self.meta_one_rx {
            if let Ok((i, m)) = rx.try_recv() {
                self.meta.insert(i, m);
                self.meta_one_rx = None;
            }
        }
        self.maybe_recheck_update();
        if self.running || matches!(self.renodx, RenodxLookup::Pending) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        let (ok_status, problems, complete) = match &self.status {
            Some(Ok(s)) => (Some(s.clone()), s.problems.clone(), s.complete()),
            Some(Err(e)) => (None, vec![e.clone()], false),
            None => (None, vec![], false),
        };

        self.pump_library(ui.ctx());
        if self.scanning || self.poster_rx.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }

        // ── top ribbon: logo · Games / Setup / About · status ────────
        egui::Panel::top("ribbon")
            .frame(
                Frame::new()
                    .fill(t::HEADER)
                    .inner_margin(Margin {
                        left: 20,
                        right: 24,
                        top: 10,
                        bottom: 10,
                    })
                    .stroke(Stroke::new(1.0, t::BORDER)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::hover());
                    logo::paint_mark(ui.painter(), rect, t::ACCENT, t::BG);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("DLSS 5").font(t::sora(16.0)).color(t::TEXT));
                        ui.label(
                            RichText::new("ONECLICK")
                                .font(t::plex_semibold(9.0))
                                .color(t::TEXT_MUTED),
                        );
                    });
                    ui.add_space(22.0);
                    let setup_enabled = self.resolved_exe.is_some();
                    for (page, label, enabled) in [
                        (Page::Games, "Games", true),
                        (Page::Setup, "Setup", setup_enabled),
                        (Page::About, "About", true),
                    ] {
                        let active = self.page == page;
                        let galley = ui.painter().layout_no_wrap(
                            label.to_owned(),
                            t::plex_medium(13.5),
                            t::TEXT,
                        );
                        let size = Vec2::new(galley.size().x + 32.0, 36.0);
                        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                        if active {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(10), t::TILE);
                            ui.painter().rect_stroke(
                                rect,
                                CornerRadius::same(10),
                                Stroke::new(1.0, t::BORDER_STRONG),
                                StrokeKind::Inside,
                            );
                        } else if resp.hovered() && enabled {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(10), t::PANEL);
                        }
                        let color = if !enabled {
                            t::TEXT_DIM
                        } else if active {
                            t::TEXT
                        } else {
                            t::TEXT_OFF
                        };
                        ui.painter()
                            .galley(rect.center() - galley.size() / 2.0, galley, color);
                        if page == Page::Games && !self.games.is_empty() {
                            ui.painter().circle_filled(
                                rect.right_top() + Vec2::new(-9.0, 9.0),
                                2.5,
                                t::ACCENT,
                            );
                        }
                        if resp.clicked() && enabled {
                            self.page = page;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        // ── Ko-fi (rightmost) ────────────────────
                        if self.kofi_icon.is_none() {
                            self.kofi_icon =
                                image::load_from_memory(include_bytes!("../assets/kofi.png"))
                                    .ok()
                                    .map(|i| {
                                        let i = i.to_rgba8();
                                        let (w, h) = i.dimensions();
                                        ui.ctx().load_texture(
                                            "kofi",
                                            egui::ColorImage::from_rgba_unmultiplied(
                                                [w as usize, h as usize],
                                                i.as_raw(),
                                            ),
                                            egui::TextureOptions::LINEAR,
                                        )
                                    });
                        }
                        let label = "Buy me a Cup of Coffee";
                        let galley = ui.painter().layout_no_wrap(
                            label.to_owned(),
                            t::plex_semibold(12.5),
                            Color32::WHITE,
                        );
                        let size = Vec2::new(galley.size().x + 46.0, 36.0);
                        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                        let fill = if resp.hovered() {
                            Color32::from_rgb(0xff, 0x74, 0x71)
                        } else {
                            KOFI_RED
                        };
                        ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
                        if let Some(tex) = &self.kofi_icon {
                            let icon = egui::Rect::from_center_size(
                                rect.left_center() + Vec2::new(20.0, 0.0),
                                Vec2::splat(18.0),
                            );
                            egui::Image::from_texture(tex)
                                .fit_to_exact_size(icon.size())
                                .paint_at(ui, icon);
                        }
                        ui.painter().galley(
                            egui::pos2(rect.left() + 36.0, rect.center().y - galley.size().y / 2.0),
                            galley,
                            Color32::WHITE,
                        );
                        if resp
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(KOFI_URL));
                        }
                        chip(
                            ui,
                            concat!("v", env!("CARGO_PKG_VERSION")),
                            t::TEXT_DIM,
                            false,
                        );
                        chip(ui, "LEAKED BUILD", t::ACCENT, true);
                        let (r, _) =
                            ui.allocate_exact_size(Vec2::new(64.0, 20.0), egui::Sense::hover());
                        ui.painter().circle_filled(
                            r.left_center() + Vec2::new(5.0, 0.0),
                            3.5,
                            t::ACCENT,
                        );
                        ui.painter().text(
                            r.left_center() + Vec2::new(14.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            "Ready",
                            t::plex_semibold(12.0),
                            t::TEXT,
                        );
                    });
                });
            });

        self.pump_update();
        match self.update.clone() {
            UpdateState::Idle => {}
            UpdateState::Available(av) => {
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.label(
                                RichText::new(format!(
                                    "Version {} is available (you have {}).",
                                    av.version,
                                    update::CURRENT
                                ))
                                .font(t::plex_medium(12.5))
                                .color(t::TEXT),
                            );
                            let upd = egui::Button::new(
                                RichText::new("Update")
                                    .font(t::plex_semibold(12.5))
                                    .color(t::BG),
                            )
                            .fill(t::ACCENT)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6));
                            if ui.add(upd).clicked() {
                                self.start_update_download(av.clone());
                            }
                            if ui.button("Later").clicked() {
                                self.update = UpdateState::Idle;
                            }
                            if ui.button("Skip this version").clicked() {
                                self.skipped_version = av.version.clone();
                                self.update = UpdateState::Idle;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.hyperlink_to(
                                    RichText::new("release notes").font(t::plex(11.5)),
                                    format!(
                                        "https://github.com/{}/releases/tag/{}",
                                        update::REPO,
                                        av.tag
                                    ),
                                );
                            });
                        });
                    });
            }
            UpdateState::Downloading(pct, msg) => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("Updating: {pct}% {msg}"))
                                .font(t::plex(12.0))
                                .color(t::TEXT_MUTED),
                        );
                    });
            }
            UpdateState::Restarting => {
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Updated. Restarting...")
                                .font(t::plex(12.0))
                                .color(t::ACCENT),
                        );
                    });
            }
            UpdateState::Failed(e) => {
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Update failed: {e}"))
                                    .font(t::plex(12.0))
                                    .color(t::DANGER),
                            );
                            if ui.button("Dismiss").clicked() {
                                self.update = UpdateState::Idle;
                            }
                        });
                    });
            }
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(t::PANEL).inner_margin(Margin {
                left: 24,
                right: 28,
                top: 18,
                bottom: 14,
            }))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                match self.page {
                    Page::Games => {
                        self.games_page(ui);
                        return;
                    }
                    Page::About => {
                        about_page(ui);
                        ui.add_space(10.0);
                        let busy = self.update_rx.is_some()
                            || !matches!(self.update, UpdateState::Idle);
                        let btn = egui::Button::new(
                            RichText::new("Check for updates")
                                .font(t::plex_medium(12.5))
                                .color(t::TEXT),
                        )
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(150.0, 34.0));
                        if ui.add_enabled(!busy, btn).clicked() {
                            // An explicit check also clears a skipped version:
                            // asking is asking.
                            self.skipped_version.clear();
                            self.checked_manually = true;
                            self.start_update_check();
                        }
                        if self.checked_manually
                            && matches!(self.update, UpdateState::Idle)
                            && self.update_rx.is_none()
                        {
                            ui.label(
                                RichText::new(concat!(
                                    "You are on the newest release (v",
                                    env!("CARGO_PKG_VERSION"),
                                    ")."
                                ))
                                .font(t::plex(12.0))
                                .color(t::TEXT_MUTED),
                            );
                        }
                        return;
                    }
                    Page::Setup => {}
                }
                // The setup panel was designed at 720 px; keep it from stretching.
                ui.set_max_width(860.0);

                // ── path row ──────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let btn_w = 2.0 * 96.0 + 8.0;
                    let field_w = ui.available_width() - btn_w - 8.0;
                    Frame::new()
                        .fill(t::BG)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 0))
                        .show(ui, |ui| {
                            ui.set_min_height(40.0);
                            ui.set_width(field_w - 24.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut self.exe_text)
                                        .frame(Frame::NONE)
                                        .font(t::mono(12.0))
                                        .text_color(t::TEXT_SOFT)
                                        .hint_text(RichText::new("Game folder or the game's .exe").color(t::TEXT_DIM))
                                        .desired_width(f32::INFINITY),
                                );
                                if r.changed() {
                                    self.refresh();
                                }
                            });
                        });
                    let start_dir = self.exe().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                    if ui.add_sized([96.0, 40.0], egui::Button::new("Game folder…")).clicked() {
                        let mut dlg = rfd::FileDialog::new().set_title("Pick the game's install folder");
                        if let Some(d) = &start_dir { dlg = dlg.set_directory(d); }
                        if let Some(p) = dlg.pick_folder() {
                            self.exe_text = p.to_string_lossy().into_owned();
                            self.refresh();
                        }
                    }
                    if ui.add_sized([96.0, 40.0], egui::Button::new("Exe…")).clicked() {
                        let mut dlg = rfd::FileDialog::new().add_filter("Executables", &["exe"]);
                        if let Some(d) = &start_dir { dlg = dlg.set_directory(d); }
                        if let Some(p) = dlg.pick_file() {
                            self.exe_text = p.to_string_lossy().into_owned();
                            self.refresh();
                        }
                    }
                });

                // ── game exe line ─────────────────────────────────
                if let Some(exe) = self.resolved_exe.clone() {
                    let base = self.input_path();
                    let short = |p: &PathBuf| -> String {
                        p.strip_prefix(&base)
                            .map(|r| r.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(RichText::new("Game exe").font(t::plex(12.0)).color(t::TEXT_MUTED));
                        if self.candidates.len() > 1 {
                            let mut pick = exe.clone();
                            egui::ComboBox::from_id_salt("exe_pick")
                                .selected_text(RichText::new(short(&pick)).font(t::mono(11.5)))
                                .show_ui(ui, |ui| {
                                    for c in &self.candidates {
                                        ui.selectable_value(&mut pick, c.clone(), short(c));
                                    }
                                });
                            if pick != exe {
                                self.resolved_exe = Some(pick);
                                self.inspect_resolved();
                            }
                        } else {
                            chip(ui, &short(&exe), t::TEXT_SOFT, false);
                        }
                        if let Some(s) = &ok_status {
                            ui.label(
                                RichText::new(format!(
                                    "{}-bit · {}{}",
                                    s.bitness,
                                    s.api.label(),
                                    s.gpu.as_ref().map(|(g, t)| format!(" · {} ({})", g.name, t.label())).unwrap_or_default()
                                ))
                                    .font(t::plex(12.0))
                                    .color(t::TEXT_DIM),
                            );
                            // Path: what was detected, overridable — a stray nvngx_dlss.dll
                            // reads as native DLSS, and #19 wanted a switch.
                            let mut choice = game::mode_override();
                            let name = |m: Option<game::Mode>| match m {
                                None => match s.mode_detected {
                                    game::Mode::Native => "Auto: native DLSS · add-on hooks the game",
                                    game::Mode::Feeder => "Auto: no DLSS · Feeder path",
                                },
                                Some(game::Mode::Native) => "Force native DLSS",
                                Some(game::Mode::Feeder) => "Force no-DLSS (Feeder)",
                            };
                            let before = choice;
                            egui::ComboBox::from_id_salt("mode_pick")
                                .selected_text(RichText::new(name(choice)).font(t::plex(12.0)).color(t::TEXT_SOFT))
                                .show_ui(ui, |ui| {
                                    for m in [None, Some(game::Mode::Feeder), Some(game::Mode::Native)] {
                                        ui.selectable_value(&mut choice, m, name(m));
                                    }
                                });
                            if choice != before && !self.running {
                                game::set_mode_override(choice);
                                self.inspect_resolved();
                            }
                        }
                    });
                }
                for p in &problems {
                    ui.label(
                        RichText::new(text::tidy(p))
                            .font(t::plex(12.0))
                            .color(t::DANGER),
                    );
                }
                if let Some(ac) = ok_status.as_ref().and_then(|s| s.anticheat) {
                    let mut on = game::ignore_anticheat();
                    let label = format!(
                        "{ac} is switched off for offline play in this game (GTA V: BattlEye unticked in the Rockstar Games Launcher, or -nobattleye) — install anyway, at my own risk"
                    );
                    let cb = egui::Checkbox::new(&mut on, RichText::new(label).font(t::plex(11.5)).color(t::TEXT_SOFT));
                    if ui.add_enabled(!self.running, cb).changed() {
                        game::set_ignore_anticheat(on);
                        self.inspect_resolved();
                    }
                }

                // ── engine chooser ───────────────────────────────
                let native = ok_status.as_ref().is_some_and(|s| s.mode == game::Mode::Native);
                if !native {
                    self.engine = Engine::ReShade;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(
                        RichText::new("INSTALL ENGINE")
                            .font(t::plex_semibold(11.0))
                            .color(t::TEXT_MUTED),
                    );
                    ui.label(
                        RichText::new(if native {
                            "— two ways to run DLSS 5 in this game, pick one"
                        } else {
                            "— this game has no DLSS of its own, so only the ReShade path can work"
                        })
                        .font(t::plex(11.0))
                        .color(t::TEXT_DIM),
                    );
                });
                {
                    let gap = 8.0;
                    let card_h = 74.0;
                    let row_w = ui.available_width();
                    let col_w = ((row_w - gap) / 2.0).floor();
                    let (row_rect, _) =
                        ui.allocate_exact_size(Vec2::new(row_w, card_h), egui::Sense::hover());
                    let left = egui::Rect::from_min_size(row_rect.min, Vec2::new(col_w, card_h));
                    let right = egui::Rect::from_min_size(
                        egui::pos2(row_rect.left() + col_w + gap, row_rect.top()),
                        Vec2::new(col_w, card_h),
                    );
                    if engine_card(
                        ui,
                        left,
                        self.engine == Engine::ReShade,
                        true,
                        "ReShade + DLSS 5 add-on",
                        &["The default. Works in every supported game.", "In game: Home → Add-ons → DLSS 5 Neural Rendering."],
                        "",
                    ) {
                        self.engine = Engine::ReShade;
                    }
                    if engine_card(
                        ui,
                        right,
                        self.engine == Engine::Opti,
                        native,
                        "OptiScaler (built-in NR pass)",
                        &["Dagherbou's fork, no ReShade. Also swaps upscalers.", "In game: Insert → enable Neural Rendering."],
                        if native {
                            ""
                        } else {
                            "Needs a game with its own DLSS — this one has none."
                        },
                    ) {
                        self.engine = Engine::Opti;
                    }
                }
                ui.add_space(2.0);

                // ── component list (status, not controls) ────────
                ui.label(
                    RichText::new("WHAT IS IN THE GAME FOLDER")
                        .font(t::plex_semibold(11.0))
                        .color(t::TEXT_MUTED),
                );
                let gap = 24.0;
                let tile_h = 44.0;
                let row_w = ui.available_width();
                let col_w = ((row_w - gap) / 2.0).floor();
                let tiles = tiles_for(ok_status.as_ref(), self.engine, self.renodx_on);
                for row in tiles.chunks(2) {
                    let (row_rect, _) =
                        ui.allocate_exact_size(Vec2::new(row_w, tile_h), egui::Sense::hover());
                    for (i, tl) in row.iter().enumerate() {
                        let x = row_rect.left() + i as f32 * (col_w + gap);
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(x, row_rect.top()),
                            Vec2::new(col_w, tile_h),
                        );
                        tile(ui, rect, tl, ok_status.as_ref());
                    }
                }

                // ── RenoDX HDR mod ────────────────────────────────
                if let Some(s) = &ok_status {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new("RENODX HDR MOD")
                                .font(t::plex_semibold(11.0))
                                .color(t::TEXT_MUTED),
                        );
                        let dim = |ui: &mut egui::Ui, text: String| {
                            ui.label(RichText::new(text).font(t::plex(11.0)).color(t::TEXT_DIM));
                        };
                        if let Some(installed) = &s.renodx_mod {
                            dim(ui, format!("— installed: {installed} (Remove takes it out too)"));
                        } else if !s.foreign_renodx.is_empty() {
                            dim(ui, format!(
                                "— already present, not installed by this tool: {} (left untouched; ReShade loads one RenoDX mod per game)",
                                s.foreign_renodx.join(", ")
                            ));
                        } else {
                            match &self.renodx {
                                RenodxLookup::Idle | RenodxLookup::Pending => dim(ui, "— looking up clshortfuse/renodx for this game…".into()),
                                RenodxLookup::NotFound => dim(ui, "— no RenoDX mod is published for this game.".into()),
                                RenodxLookup::Failed(e) => dim(ui, format!("— lookup failed: {e}")),
                                RenodxLookup::Found(m) => {
                                    let label = format!("Also install {} — {}", m.file, m.status_label());
                                    let cb = egui::Checkbox::new(&mut self.renodx_on, RichText::new(label).font(t::plex(12.0)).color(t::TEXT_SOFT));
                                    ui.add_enabled(!self.running, cb).on_hover_text(
                                        "Game-specific HDR / tone-mapping mod from the RenoDX project. Loads beside the DLSS 5 add-on (different add-on name, different settings section). Turn Windows AutoHDR / RTX HDR off to avoid double tone mapping.",
                                    );
                                    if self.engine == Engine::Opti {
                                        dim(ui, "— ReShade goes in as ReShade64.dll, loaded by OptiScaler (LoadReshade=true)".into());
                                    }
                                    if !m.note.is_empty() {
                                        dim(ui, format!("— {}", m.note));
                                    }
                                }
                            }
                        }
                    });
                }

                // ── actions ───────────────────────────────────────
                let can_run = ok_status.is_some() && problems.is_empty() && !self.running;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let install = egui::Button::new(RichText::new("Install DLSS 5").font(t::plex_semibold(14.0)).color(t::BG))
                        .fill(t::ACCENT)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(180.0, 42.0));
                    if ui.add_enabled(can_run, install).clicked() {
                        self.start(None);
                    }
                    let remove = egui::Button::new(RichText::new("Remove").font(t::plex_medium(13.0)).color(t::TEXT_OFF))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(90.0, 42.0));
                    if ui.add_enabled(ok_status.is_some() && !self.running, remove).clicked() {
                        self.confirm_remove = true;
                    }
                    let diag = egui::Button::new(
                        RichText::new("Diagnose").font(t::plex_medium(13.0)).color(t::TEXT_OFF),
                    )
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(96.0, 42.0));
                    if ui
                        .add_enabled(ok_status.is_some() && !self.running, diag)
                        .on_hover_text(
                            "Reads this game's ReShade and feed logs and says why neural rendering is or is not running. Play the game first.",
                        )
                        .clicked()
                    {
                        self.run_diagnose();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let msg = if self.running || !self.progress_msg.is_empty() {
                            self.progress_msg.clone()
                        } else if complete {
                            "Everything is in place.".to_owned()
                        } else {
                            String::new()
                        };
                        let color = if msg.starts_with("Failed") { t::DANGER } else { t::ACCENT };
                        ui.label(RichText::new(msg).font(t::plex_medium(12.0)).color(color));
                    });
                });

                // ── progress ──────────────────────────────────────
                let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, CornerRadius::same(2), t::BORDER);
                let frac = if self.running { self.progress as f32 / 100.0 } else if self.progress == 100 || complete { 1.0 } else { 0.0 };
                if frac > 0.0 {
                    let mut fill = bar;
                    fill.set_width(bar.width() * frac);
                    ui.painter().rect_filled(fill, CornerRadius::same(2), t::ACCENT);
                }

                // ── log ───────────────────────────────────────────
                let hint_h = 34.0;
                let log_h = (ui.available_height() - hint_h - 12.0).max(80.0);
                Frame::new()
                    .fill(t::BG)
                    .stroke(Stroke::new(1.0, t::BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(log_h - 20.0);
                        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.set_width(ui.available_width());
                            for l in &self.log {
                                match l {
                                    LogLine::Step(s) => {
                                        let (idx, rest) = s.split_once(' ').unwrap_or(("", s));
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            ui.label(RichText::new(idx).font(t::mono(11.5)).color(t::TEXT_DIM));
                                            ui.label(RichText::new(rest).font(t::mono(11.5)).color(t::TEXT_MUTED));
                                        });
                                    }
                                    LogLine::Ok(s) => { ui.label(RichText::new(format!("      {s}")).font(t::mono(11.5)).color(t::ACCENT)); }
                                    LogLine::Fail(s) => { ui.label(RichText::new(format!("      {s}")).font(t::mono(11.5)).color(t::DANGER)); }
                                    LogLine::Plain(s) => { ui.label(RichText::new(s).font(t::mono(11.5)).color(t::TEXT)); }
                                }
                            }
                        });
                    });

                ui.label(RichText::new(
                    "After install, in game: press Home for the ReShade overlay, open the DLSS 5 Neural Rendering panel and enable it. \
                     Keep MSAA/SSAA off. Check dlss5-feed.log next to the exe for 'feature ready'.")
                    .font(t::plex(11.0)).color(t::TEXT_DIM));
            });

        // ── dialogs ───────────────────────────────────────────────
        if self.confirm_remove {
            egui::Window::new(RichText::new("Remove DLSS 5 files").font(t::sora(14.0)))
                .collapsible(false).resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label("Remove the DLSS 5 files from this game?

Remove takes out what this tool added; ReShade stays.
Remove incl. ReShade also deletes ReShade (dxgi.dll, ini files, reshade-shaders) - refused if any add-on or shader this tool did not install is still there.");
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() { self.confirm_remove = false; self.start(Some(false)); }
                        if ui.button("Remove incl. ReShade").clicked() { self.confirm_remove = false; self.start(Some(true)); }
                        if ui.button("Cancel").clicked() { self.confirm_remove = false; }
                    });
                });
        }
        if let Some(err) = self.last_error.clone() {
            egui::Window::new(RichText::new("DLSS5oneclick").font(t::sora(14.0)))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.set_max_width(480.0);
                    ui.label(RichText::new(&err).color(t::DANGER));
                    if ui.button("OK").clicked() {
                        self.last_error = None;
                    }
                });
        }
    }
}

pub fn run() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 780.0])
        .with_min_inner_size([880.0, 620.0])
        .with_title(concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION")));
    if let Some(icon) = logo::icon_data() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
