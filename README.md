# Vista TV

A fast, local-first desktop IPTV player. Built with **Tauri 2 + Svelte 5 + Rust**,
with **libmpv** for playback (HEVC/4K/HDR, hardware decode). Fullscreen/immersive mode
(F / double-click / Esc), keyboard shortcuts (Space, ←/→), and buffered streaming with
auto-reconnect. (App identifier remains `com.arun4.vista-iptv` to preserve existing data.)

## Download

Grab the latest installers from **[Releases](https://github.com/Carnaticr/VistaTV/releases)**:

| Platform | File | Notes |
|---|---|---|
| Windows x64 | `Vista.TV_…_x64-setup.exe` | SmartScreen warns (unsigned) → **More info → Run anyway** |
| Windows x64 (MSI) | `Vista.TV_…_x64_en-US.msi` | For silent/enterprise deploy |
| macOS (Apple Silicon) | `Vista.TV_…_aarch64.dmg` | **Self-contained** (libmpv bundled, no Homebrew). Unsigned → right-click → **Open**. CI-built, not yet verified on hardware |

All data (channels, favorites, recents, Xtream credentials) stays on your machine.

## Status

**Milestone 1 — mpv playback proof (done).**
**Milestone 2 — mpv embedded inside the app window (done).**
**Milestone 3 — playlist engine + channel browser (done).**
**Milestone 4 — multi-source, Xtream login, favorites & recents (done).**

mpv is embedded via [`tauri-plugin-libmpv`](https://github.com/nini22P/tauri-plugin-libmpv):
it passes the Tauri window's `HWND` to mpv as `wid`, and the WebView2 is made transparent
so the Svelte controls overlay the video. The video is inset to the stage area (right of the
channel sidebar, above the transport bar) via `setVideoMarginRatio`. Play / pause / seek / stop /
volume work, with live `time-pos` / `duration` / `media-title` from mpv property observers.

The playlist engine parses extended-M3U in Rust and indexes channels in SQLite (FTS5) for
instant prefix search and group filtering. Channels come from named **sources**: an M3U
URL/file, or an **Xtream Codes** provider login (host + username + password). Sources can be
refreshed or removed; search runs across all sources or a single one. **Favorites** (starred)
and **Recents** (last 50 played) are keyed by stream URL so they survive re-imports.

- `src/routes/+page.svelte` — channel browser (sources, add/sign-in, tabs, stars) + player UI
- `src-tauri/src/playlist.rs` — extended-M3U parser (+ unit tests)
- `src-tauri/src/xtream.rs` — Xtream Codes API client (auth + live streams)
- `src-tauri/src/db.rs` — SQLite/FTS5 store + source / search / favorites / recents commands (+ tests)
- `src-tauri/lib/` — runtime mpv libraries, bundled via `bundle.resources`

The DB lives at `%APPDATA%/com.arun4.vista-iptv/vista.db`. Xtream credentials are stored
locally in that DB only (no cloud, no account).

**Milestone 5 (next):** EPG (XMLTV now/next), and channel-list virtualization for very large
(100k+) playlists.

## Prerequisites

- Node 20+, Rust (MSVC toolchain), `cargo-tauri`
- Windows: WebView2 runtime (ships with Windows 11)

## Runtime libraries (required)

The plugin loads two dynamic libraries at runtime from `src-tauri/lib/`:

- `libmpv-2.dll` — the mpv core (LGPL build)
- `libmpv-wrapper.dll` — the plugin's FFI wrapper

These are **not** committed as source; fetch them with the plugin's setup script:

```sh
npx tauri-plugin-libmpv-api setup-lib
```

`tauri.conf.json` bundles them (`bundle.resources: ["lib/**/*"]`) and enables the
transparent window (`app.windows[0].transparent: true`) the embedding requires.

## Develop

```sh
npm install
npx tauri-plugin-libmpv-api setup-lib   # first time only — downloads the DLLs
npm run tauri dev
```

Press **Play** with the default test HLS URL to confirm embedded playback.
