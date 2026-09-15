# DLSS5oneclick

<p>
  <a href="https://github.com/faisalkindi/DLSS5oneclick/releases/latest"><img src="https://img.shields.io/github/v/release/faisalkindi/DLSS5oneclick?style=flat-square&color=2878D0&label=Download" alt="Download"></a>
  <img src="https://img.shields.io/github/downloads/faisalkindi/DLSS5oneclick/total?style=flat-square&color=16A34A&label=Downloads" alt="Downloads">
  <img src="https://img.shields.io/github/stars/faisalkindi/DLSS5oneclick?style=flat-square&color=EAB308&label=Stars" alt="Stars">
  <a href="https://ko-fi.com/kindiboy"><img src="https://img.shields.io/badge/Support-Ko--fi-FF5E5B?style=flat-square&logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
</p>

One button that sets up the **leaked DLSS 5 neural-rendering build** in any DirectX 11/12 game, with or without DLSS of its own. Single native Windows exe, no runtime. Everything it installs is downloaded from the projects that made it; the only third-party content inside the exe is three SIL-OFL fonts.

Download: [latest release](https://github.com/faisalkindi/DLSS5oneclick/releases/latest) → `dlss5oneclick.exe`.

## Two paths, picked automatically

| The game | What gets installed |
|---|---|
| **Ships its own DLSS** (an `nvngx_dlss.dll` this tool did not place, or Streamline `sl.*.dll`, `nvngx_dlssg/dlssd.dll`, anywhere up to four folders deep, or under an Unreal project's `Plugins` tree) | ReShade add-on build + the DLSS 5 add-on (`renodx-dlss5.addon64`, `nvngx_dlssnr.dll`). The add-on hooks the game's own NGX calls directly. **DX11 games** also get [dlss5-bridge](https://github.com/NIGos/dlss5-bridge), which replays the D3D11 DLSS calls on a private D3D12 device so the add-on can see them. No Feeder, no LumeniteFX; a Feeder left over from an earlier run is removed. |
| **Has no DLSS** | The full Feeder path below: ReShade + shader headers + DLSS5-Feeder + LumeniteFX + the DLSS 5 add-on + config. |

**Engine choice for games with native DLSS** (two cards at the top of the window, the second one greyed out with a reason when the game has no DLSS): the default engine is ReShade + the RenoDX add-on. A second engine — [Dagherbou's OptiScaler_DLSSNR fork](https://github.com/Dagherbou/OptiScaler_DLSSNR) (OptiScaler with a built-in Neural Rendering pass, colour composition from RenoDX under MIT) — can be picked in the GUI or with `--engine=opti`: the tool extracts the fork's release into the game as `dxgi.dll`, adds `nvngx_dlssnr.dll`, and records a manifest so Remove takes it out cleanly. In game, Insert opens the OptiScaler overlay; Neural Rendering is off by default there. The two engines cannot share a game (both load as dxgi.dll). Note the fork targets the unpatched model: on the driver's own DLL that means RTX 50; with the `310.8.SF` model this tool installs, older RTX generations may work but are untested there.

**Standalone AIO engine (experimental, since 0.13.15).** A third card, for 64-bit games ReShade can reach from `dxgi.dll`: [kibblerz's DLSS5 ReShade AIO](https://github.com/kibblerz/DLSS5-Reshade-AIO) (Apache-2.0) is one ReShade add-on that runs neural rendering, DLSS super resolution / DLAA and frame generation itself, so it takes the place of the Feeder and the RenoDX add-on together — frame generation in a game that never had it. Pick it in the GUI or with `--engine=aio`: the tool installs ReShade, extracts the 64-bit AIO zip beside the exe (recorded in a manifest so Remove takes it out), adds `nvngx_dlssnr.dll`, and places `nvngx_dlss.dll` and `nvngx_dlssg.dll` from NVIDIA's repository when the game has none. In game: turn the game's own upscaling, anti-aliasing and frame generation off, run windowed (the add-on's own recommendation), then Home → Add-ons → Standalone DLSS-NR + SR. Switching a game back to the ReShade engine removes the AIO first, so ReShade never loads two neural consumers. The 32-bit package is not wired.

**RenoDX HDR mod (optional, since 0.8.0; engine-independent since 0.8.1).** The [RenoDX](https://github.com/clshortfuse/renodx) project publishes game-specific HDR / tone-mapping mods as ReShade add-ons (`renodx-<game>.addon64`). When the tool recognises the game (Steam app id from the library's `appmanifest_*.acf`, else the folder / exe name, matched against RenoDX's `games-index.json` and its wiki mod list), a **RenoDX HDR mod** checkbox appears with the mod's status (working / in progress) and the wiki's note for that game; `--renodx` does the same on the command line. It loads beside the DLSS 5 add-on: ReShade only refuses two add-ons with the same name, and the game mods register as "RenoDX" while the DLSS 5 add-on registers as "DLSS 5 Neural Rendering"; their settings live in different `ReShade.ini` sections (`[renodx-preset1]` vs `[RenoDX.DLSS5]`). Verified in Clair Obscur: Expedition 33 and Dragon's Dogma 2 (both add-ons registered in `ReShade.log`). Exactly **one** RenoDX game mod per game — a second one is refused by ReShade and both would write the same keys — so the tool refuses when another `renodx-*.addon64` is already there, and Remove only deletes the one it recorded. The link the wiki gives (often a maintainer's fork snapshot) wins over the main-repo snapshot build; games the wiki lists as Nexus/Discord-only get the snapshot build with a note. Turn Windows AutoHDR / RTX HDR off with these mods (double tone mapping). The generic Unreal/Unity fallbacks RHI offers are deliberately not installed. The checkbox is independent of the engine: on the **OptiScaler engine** the mod still needs ReShade, so the tool adds the ReShade DLL as `ReShade64.dll` and sets `[Plugins] LoadReshade=true` in `OptiScaler.ini` — the method OptiScaler's own ini documents for running ReShade add-ons beside it. That combination follows the documentation but has not been run in a game by the author; if it crashes, untick the mod or switch engines.

**RE Engine games** (Resident Evil 2/3/4/7/8/Requiem, Devil May Cry 5, Monster Hunter Rise/Wilds, Street Fighter 6, Dragon's Dogma 2, Pragmata — anything with `re_chunk_000.pak` next to the exe) crash under ReShade unless praydog's [REFramework](https://github.com/praydog/REFramework) is loaded first. Since 0.8.0 the tool installs its monolithic nightly `dinput8.dll` as the first step in those games (only the DLL, as its release notes insist) and Remove takes it out again; a `dinput8.dll` the tool did not place is left alone.

**Wrong path?** The DLSS detection is a folder scan, so a stray `nvngx_dlss.dll` (left by another tool, or by this one before it started marking its own copy) makes a game without DLSS look like a native-DLSS game. The dropdown next to the game line (**Auto / Force no-DLSS (Feeder) / Force native DLSS**), `--mode=feeder|native`, or `DLSS5ONECLICK_MODE` overrides it; the auto line still shows what was detected.

**Hybrid machines (laptop iGPU + dGPU, or an AMD/Intel display adapter alongside the NVIDIA card).** Windows decides which GPU a process starts on, and a process started on the integrated GPU has no NGX at all — every `NVSDK_NGX_D3D12_Init` answers `0xBAD00001` (FeatureNotSupported) no matter how correct the install is. Install now writes the same preference the Settings app writes (`GpuPreference=2;` under `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`) for the game exe, and for `host64\dlss5-feed-host64.exe` on a 32-bit game. Remove takes it away again, but only when it is still exactly what was written. `--check` prints the machine's adapters and the current preference.

**Bug reports: one zip.** The **Save report** button (or `dlss5oneclick.exe "<game folder>" --report`) writes `dlss5oneclick-report-<game>-<time>.zip` to your Desktop with the game folder listing, `ReShade.log` / `dlss5-feed.log` / `OptiScaler.log` / REFramework's log (last 4 MB of each), the inis this tool wrote, the version sidecars, and Diagnose's output. Attach that to an issue instead of hunting for files.

DX11 vs DX12 is read from the exe's import table, then from the engine DLLs next to it (`UnityPlayer.dll`, ...), and a `D3D12\D3D12Core.dll` (DirectX Agility SDK redist) next to the exe counts as DX12 even when only `d3d11.dll` is imported (RE Engine). When nothing says, DX12 is assumed and the status line says so; the dropdown beside the DLSS-mode one (or `--api=dx11|dx12`) forces it when the guess is wrong. `dlss5oneclick.exe "<game folder>" --check` prints the detected mode, API and plan without installing anything.

### The no-DLSS path

**Every component is taken from its project's latest release.** For DLSS5-Feeder that can be a tag named `-beta`: upstream publishes builds it means people to run with `prerelease=false` (v0.13.1-beta.1, v0.12.1-beta.2) while flagging the ones it does not, and those carry fixes the last plain-numbered release lacks. Install names the tag it took, with `(beta)` after it, so you always know what you are running.

It does, in order, exactly what the [DLSS5-Feeder README](https://github.com/jlrouzies-fr/DLSS5-Feeder#install-for-a-64-bit-game) tells you to do by hand:

| Step | What | From |
|---|---|---|
| 1 | ReShade **with add-on support**, dropped as `dxgi.dll` | `ReShade_Setup_<ver>_Addon.exe` on [reshade.me](https://reshade.me) (DLL pulled straight out of the installer, nothing is run) |
| 2 | `ReShade.fxh`, `ReShadeUI.fxh`, `DrawText.fxh` into `reshade-shaders\Shaders` (the setup exe has only the DLLs; every shader below includes `ReShade.fxh`) | [crosire/reshade-shaders](https://github.com/crosire/reshade-shaders/tree/slim/Shaders) (`slim` branch) |
| 3 | `dlss5-feed.addon64` + `DLSS5_Feed.fx` | [jlrouzies-fr/DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder/releases/latest) |
| 4 | Motion-vector provider: `lumenite_*.fx`, `include\*.fxh`, `lumenite_bluenoise256.png` | [umar-afzaal/LumeniteFX](https://github.com/umar-afzaal/LumeniteFX) (`mainline` branch) |
| 5 | `renodx-dlss5.addon64` (the leaked DLSS 5 add-on, closed-source and community-distributed), `nvngx_dlssnr.dll` (its neural-rendering model), `nvngx_dlss.dll` (DLSS runtime; the Feeder's NGX session fails without one next to the game, so it is always placed and marked with a `nvngx_dlss.dll.dlss5oneclick` sidecar) | [RankFTW/rhi-repo](https://github.com/RankFTW/rhi-repo/releases) releases (`renodx-dlss5-*`, `dlssnr-*`, `dlss-*`) |
| 6 | `ReShade.ini` gets `PreprocessorDefinitions=DLSS5_MV_PROVIDER=3`; `ReShadePreset.ini` enables `Lumenite_Kernel` **above** `DLSS5_Feed` | written by this tool, existing keys preserved |

Every file is downloaded from its upstream at install time; a re-run only fetches what is missing.

## Use

1. Run `dlss5oneclick.exe` (single native binary, no runtime needed).
2. The **Games** page lists every installed game it can find — Steam (all library folders), Epic Games, GOG and Xbox / Game Pass — newest install first, with the store's own artwork (Steam's cached library art or its CDN, Xbox's tile logo, the exe icon otherwise) and, once inspected, a DirectX 11/12 chip plus "DLSS / add-on" status per card. Click a poster to open its **Setup** page. **Add a folder** / **Add a game** still take any folder or exe by hand, and a game added that way is remembered: it comes back in an **Added by you** section at the top of the list on every later start (right-click its card to forget it). **Rescan** re-reads the stores. `--list-games` prints the same list headless.
3. On the Setup page, pick the game's **folder** (or its `.exe`) - the game exe is detected automatically (the folder and two levels below it are searched, so `bin\x64_dx12\` and Unreal `Binaries\Win64\` layouts work; Unity crash handlers, Unreal helpers and redist installers are skipped; a `*-Shipping.exe` is preferred). If several candidates remain, a dropdown lets you choose. The list shows what is already present.
3. **Install DLSS 5**.
4. In game: **Home** opens ReShade → **Add-ons** tab → **DLSS 5 Neural Rendering** panel → enable it. Keep the game's MSAA/SSAA off. On games with their own DLSS the Home tab says "No effect files found" — expected, no shaders are needed there; the panel lives on the Add-ons tab.

**F6** toggles neural rendering on/off, **F5** saves the add-on's screenshot (both are the add-on's own hotkeys). On the Feeder path, `dlss5-feed.log` next to the game exe should show `feature ready … DLAA` and `DLSS5_MV_PROVIDER=3 (LumeniteFX Kernel) -> Lumenite_Kernel (enabled)`.

CLI: `dlss5oneclick.exe "C:\Games\Foo"` (folder or exe) / `--renodx` (also install the game's RenoDX HDR mod) / `--check` (detect only, also names the RenoDX mod it would install) / `--diagnose` (read the game's ReShade/feed logs and say why neural rendering is or is not running) / `--remove` (headless, prints progress).

## Updates

On start the tool looks at `github.com/faisalkindi/DLSS5oneclick/releases/latest` (a redirect, no API) in the background. If a newer version exists, a bar offers **Update / Later / Skip this version**; nothing is downloaded unless you press Update. Update fetches the release exe, checks it is a real executable, swaps it in place of the running one (the old file is kept as `dlss5oneclick.exe.old` until the next start) and restarts. `dlss5oneclick.exe --update` does the same from the command line.

## Downloads and GitHub

Every component comes from GitHub releases. Since 0.5.1 the tool reads the public release **pages** (no API), so it is not subject to GitHub's 60-requests-per-hour API cap that caused `HTTP 403 Forbidden` for people installing into many games. If you set a `GITHUB_TOKEN` environment variable it is used for the API path first. Where github.com itself is unreachable (some countries block it), a proxy or VPN is the only way — the files exist nowhere else this tool trusts.

## GPU support

The tool reads the installed display adapters from the registry and refuses up front on anything that cannot run the model: non-NVIDIA cards (NGX does not exist there) and NVIDIA cards without tensor cores (GTX/GT/MX). Among RTX cards, expect very different costs — the DLSS 5 model is FP8 with RTX-50-only kernels; the `310.8.SF` build the tool installs adds patched binaries for RTX 40 and an FP16 path for RTX 20/30. The status line shows the tier: RTX 50 full speed · RTX 40 moderate cost · RTX 20/30 heavy cost. Virtual/remote adapters (Hyper-V GPU-P, RDP, VMs) are treated as unknown and allowed. If your card is misdetected, set `DLSS5ONECLICK_SKIP_GPU_CHECK=1` to bypass the refusal.

## Verifying a download

Each release's notes carry the SHA-256 of the attached `dlss5oneclick.exe`. Check yours with `certutil -hashfile dlss5oneclick.exe SHA256` (or PowerShell `Get-FileHash`). Only this repository's Releases page and the linked Nexus Mods page are legitimate sources — "DLSS 5 manager/one-click" executables from other repositories, videos or websites are not this tool, and at least one such repository distributes a 500 MB binary with no source at all.

## Windows Defender / SmartScreen

The exe is not code-signed (no publisher certificate), it is new, and it downloads DLLs into game folders — three things Windows heuristics dislike. Expect a SmartScreen "unknown publisher" prompt; if Defender quarantines the exe or, worse, the add-on files it placed in a game, restore them from Protection history, add the game folder as an exclusion, and re-run Install (it re-fetches only what is missing). Every release is built from the public source in this repository.

## Known issues

- **Feeder path + exclusive fullscreen.** Every focus change (alt-tab) makes the game recreate its swapchain; DLSS5-Feeder rebuilds its DLSS feature and can crash inside `CreateFeature` on that rebuild ([Feeder issue #16](https://github.com/jlrouzies-fr/DLSS5-Feeder/issues/16), upstream). The game keeps rendering, DLSS 5 stops. Use borderless/windowed; raising `create_delay` in `dlss5-feed.cfg` helps. Seen on Fell & Sell; the same game ran 16,000+ frames without a crash in borderless.
- **Frame cost.** Neural rendering at native 4K adds several milliseconds. With v-sync on at 60 Hz that shows up as a hard drop to 30 fps. Turn v-sync off, or lower `work_resolution` in `dlss5-feed.cfg` (Feeder path, D3D11 games).
- **API detection can come back unknown** (monolithic Unreal exes load D3D at runtime, nothing static to read). The tool then assumes DX12 and says so; a DX11 game in that state would miss the bridge. `--check` shows what was detected.
- **Cyberpunk 2077 on the ReShade engine: FPS drops, picture unchanged.** The game drives DLSS from several threads and NGX features (Ray Reconstruction is a separate one); the RenoDX add-on reads that as many independent passes and re-creates its feature constantly — 18 times in four minutes in one log, under one evaluated frame a second ([#95](https://github.com/faisalkindi/DLSS5oneclick/issues/95)). Diagnose reports the churn. **Use the OptiScaler engine for Cyberpunk** — the pass runs inside the upscaler as one feature; confirmed working by the reporter.
- The DLSS 5 add-on and its model are a leaked, closed-source build from rhi-repo; the tool cannot vouch for them. **The add-on build installed by default is 4.70**, not the newest: 5.2.1 (September 11) came back broken in four games within three days (Dragon's Dogma 2 crash at the first evaluate, RDR2 blown-out colour, Elden Ring under Proton white, flashing in Lunar Eclipse) where 4.70 was what every reporter had working. "Try the newest add-on build" on the Setup page (or `--addon=latest`) opts into the newest; the classic tick pins 4.55; `--addon=<tag>` or `DLSS5ONECLICK_RENODX_TAG` pins any build. An install on 5.2.1 is moved to 4.70 on the next Install unless the newest tick is on.

## Not handled

- **Game Pass / Microsoft Store copies.** Windows protects those installs: the real executable is locked, the folder refuses writes, and a `dxgi.dll` beside a packaged exe never loads. The tool recognises them (`MicrosoftGame.config` above the exe) and says so, including whether the game ships DLSS natively (read from its folder); it used to inspect the `gamelaunchhelper.exe` stub and call every one "no DLSS". The same game from Steam, Epic or GOG works.
- **32-bit games** are handled since 0.10.0 (beta, Feeder path only): the Feeder's `dlss5-feed.addon32` and a 32-bit ReShade go beside the exe, and a `host64\` folder gets `dlss5-feed-host64.exe`, a 64-bit ReShade, the DLSS 5 add-on and the two NVIDIA DLLs — the layout the [Feeder README](https://github.com/jlrouzies-fr/DLSS5-Feeder#install-for-a-32-bit-game-beta) describes. The 32-bit add-on supports Direct3D 11 only; the helper's DLSS 5 panel is shown inside the game from the Feeder's Add-ons page. Verified here only as a file layout, not in a game — reports welcome on #17.
- **DirectX 9** and **Vulkan** games — different proxy / a Vulkan layer; refused.
- Online games — the tool refuses when it finds Easy Anti-Cheat, BattlEye or GameGuard files in the install (ReShade add-on injection is exactly what they flag: kick at best, ban at worst). Overwatch, Valorant and League (Blizzard/Riot anti-cheat, no marker files) are refused by exe name; Overwatch additionally blocks unsigned DLLs, so add-ons fail there with error `0x80090006`. Some games let you switch the anti-cheat off for offline play (GTA V: untick *Enable BattlEye* in the Rockstar Games Launcher, or launch with `-nobattleye`; Rockstar's own FAQ says BattlEye is only needed for GTA Online). For those, a checkbox under the warning — or `--ignore-anticheat` on the command line, or `DLSS5ONECLICK_IGNORE_ANTICHEAT=1` — installs anyway, at your own risk: do it only if the anti-cheat really is off, and never take that install online.

## Development

Rust 2021, single crate. GUI is egui/eframe; HTTP is reqwest (rustls); archives via the `zip` crate.

```
cargo test
cargo build --release   # target/release/dlss5oneclick.exe
```

Tests use local fakes only; no network. Verified 2026-08-31: full live installs against dummy game folders (both paths), and detection against real installs — Fell & Sell (Unity, DX11, no DLSS → Feeder), Fatal Claw (Unreal, DX11 + DLSS → native + bridge), Mortal Shell 2 (Unreal + DLSS → native), The Witcher 3 (`bin\x64_dx12`, native DX12), Jotunnslayer and Trails in the Sky (DX11 + DLSS → native + bridge). DLSS 5 confirmed running in Fell & Sell (`feature ready … DLAA`, NR evaluating, F6 toggling).

## Credits

This tool only automates other people's work. The credit belongs to:

- **[crosire](https://github.com/crosire)** — [ReShade](https://reshade.me) and [reshade-shaders](https://github.com/crosire/reshade-shaders), the injection framework everything here runs inside.
- **[jlrouzies-fr](https://github.com/jlrouzies-fr)** — [DLSS5-Feeder](https://github.com/jlrouzies-fr/DLSS5-Feeder), the add-on that builds a DLSS contract from ReShade depth + motion vectors, and the install guide this tool follows step by step.
- **[Afzaal (Kaidō)](https://github.com/umar-afzaal)** — [LumeniteFX](https://github.com/umar-afzaal/LumeniteFX), the motion-vector provider (Kernel 2.0).
- **[Simple Icons](https://simpleicons.org)** (CC0 1.0) — the Steam, Xbox, Epic Games, GOG and Ko-fi marks in the window; the marks themselves are trademarks of their owners.
- **[praydog](https://github.com/praydog)** — [REFramework](https://github.com/praydog/REFramework), installed first in RE Engine games.
- **[clshortfuse](https://github.com/clshortfuse)** and the RenoDX community — [RenoDX](https://github.com/clshortfuse/renodx), which the DLSS 5 neural-rendering add-on is built on.
- **[RankFTW](https://github.com/RankFTW)** — [RHI](https://github.com/RankFTW/RHI) and the [rhi-repo](https://github.com/RankFTW/rhi-repo) releases that host the DLSS 5 add-on and the NVIDIA runtimes.
- **NVIDIA** — DLSS 5 itself and the `nvngx_dlssnr.dll` / `nvngx_dlss.dll` runtimes.
- **DSOGaming** — the [article](https://www.dsogaming.com/articles/heres-how-you-can-install-dlss-5-to-all-dx9-dx10-dx11-dx12-and-vulkan-games/) that put the pieces together and started this.
- **[Dagherbou](https://github.com/Dagherbou)** — [OptiScaler_DLSSNR](https://github.com/Dagherbou/OptiScaler_DLSSNR), the OptiScaler fork with the built-in Neural Rendering pass, and the **[OptiScaler team](https://github.com/optiscaler/OptiScaler)** it builds on (GPL-3).
- **[kibblerz](https://github.com/kibblerz)** — [DLSS5-Reshade-AIO](https://github.com/kibblerz/DLSS5-Reshade-AIO), the standalone add-on behind the AIO engine.
- **[NIGos](https://github.com/NIGos)** — [dlss5-bridge](https://github.com/NIGos/dlss5-bridge), which lets the DLSS 5 add-on work in D3D11 games that have their own DLSS.
- **[emilk](https://github.com/emilk)** — [egui / eframe](https://github.com/emilk/egui), the UI toolkit.
- Fonts: [Sora](https://github.com/sora-xor/sora-font) by the Sora project, [IBM Plex Sans](https://github.com/IBM/plex) by IBM, [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) by JetBrains — all SIL OFL.

## License

MIT for this tool. Each downloaded component keeps its own license: ReShade BSD-3; DLSS5-Feeder — see its repo; LumeniteFX — AGNYA; dlss5-bridge MIT; the DLSS 5 add-on (`renodx-dlss5.addon64`) — closed source, no license published; NVIDIA runtimes — NVIDIA's terms.
