<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import {
    init,
    destroy,
    command,
    setProperty,
    observeProperties,
    setVideoMarginRatio,
    type MpvConfig,
    type MpvObservableProperty,
  } from "tauri-plugin-libmpv-api";

  // ---- Types ----
  type Channel = {
    id?: number;
    source_id?: number;
    name: string;
    url: string;
    group: string;
    tvg_id?: string;
    tvg_logo: string;
    is_fav: boolean;
  };
  type GroupCount = { name: string; count: number };
  type Source = {
    id: number;
    name: string;
    kind: string;
    location: string;
    username: string;
    count: number;
    refreshed_at: number | null;
  };

  // ---- Player state ----
  let error = $state("");
  let initError = $state("");
  let ready = $state(false);
  let paused = $state(true);
  let position = $state(0);
  let duration = $state(0);
  let volume = $state(100);
  let nowPlaying = $state<Channel | null>(null);

  // ---- Browser state ----
  type View = "channels" | "favorites" | "recents";
  let view = $state<View>("channels");
  let sources = $state<Source[]>([]);
  let sourceId = $state<number | null>(null);
  let query = $state("");
  let group = $state("");
  let groups = $state<GroupCount[]>([]);

  let channels = $state<Channel[]>([]);
  let favorites = $state<Channel[]>([]);
  let recents = $state<Channel[]>([]);
  let offset = $state(0);
  let hasMore = $state(false);
  let loading = $state(false);
  const PAGE = 200;

  // ---- Add-source form ----
  let showAdd = $state(false);
  let addKind = $state<"m3u" | "xtream">("m3u");
  let addName = $state("");
  let addLocation = $state("https://iptv-org.github.io/iptv/categories/news.m3u");
  let addHost = $state("");
  let addUser = $state("");
  let addPass = $state("");
  let adding = $state(false);
  let busy = $state(false);

  const items = $derived(
    view === "channels" ? channels : view === "favorites" ? favorites : recents
  );

  // ---- mpv setup ----
  const OBSERVED = [
    ["pause", "flag"],
    ["time-pos", "double", "none"],
    ["duration", "double", "none"],
    ["volume", "double"],
  ] as const satisfies MpvObservableProperty[];

  // macOS: gpu-next wants Vulkan/MoltenVK which isn't in the bundle — use
  // mpv's native default vo (gpu/cocoa) there instead.
  const isMac = navigator.userAgent.includes("Mac");

  const mpvConfig: MpvConfig = {
    initialOptions: {
      vo: "gpu-next",
      // macOS: keep VO logging on this round to confirm MoltenVK initializes.
      ...(isMac ? { terminal: "yes", "msg-level": "vo=v" } : {}),
      hwdec: "auto-safe",
      "keep-open": "yes",
      "force-window": "yes",
      // Buffering for smooth IPTV playback (reduce stutter/lag).
      cache: "yes",
      "cache-secs": "30",
      "demuxer-max-bytes": "150MiB",
      "demuxer-max-back-bytes": "50MiB",
      "demuxer-readahead-secs": "20",
      "cache-pause": "yes",
      "cache-pause-wait": "1",
      "network-timeout": "30",
      // Auto-reconnect on dropped live connections.
      "stream-lavf-o": "reconnect=1,reconnect_streamed=1,reconnect_delay_max=5",
      "user-agent": "VLC/3.0.20 LibVLC/3.0.20",
    },
    observedProperties: OBSERVED,
  };

  let unlisten: (() => void) | null = null;
  let sidebarEl: HTMLElement;
  let transportEl: HTMLElement;

  // ---- Fullscreen / immersive ----
  let fullscreen = $state(false);
  let controlsVisible = $state(true);
  let hideTimer: ReturnType<typeof setTimeout>;

  async function updateMargins() {
    if (!ready) return;
    if (fullscreen) {
      // Immersive: video fills the whole screen.
      await setVideoMarginRatio({ left: 0, right: 0, top: 0, bottom: 0 }).catch(() => {});
      return;
    }
    const w = window.innerWidth || 1;
    const h = window.innerHeight || 1;
    const left = sidebarEl ? sidebarEl.offsetWidth / w : 0;
    const bottom = transportEl ? transportEl.offsetHeight / h : 0;
    await setVideoMarginRatio({ left, right: 0, top: 0, bottom }).catch(() => {});
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    if (fullscreen) hideTimer = setTimeout(() => (controlsVisible = false), 2500);
  }
  function revealControls() {
    controlsVisible = true;
    scheduleHide();
  }
  async function toggleFullscreen() {
    fullscreen = !fullscreen;
    await getCurrentWindow().setFullscreen(fullscreen).catch((e) => (error = String(e)));
    revealControls();
    // Recompute margins once the window has resized.
    requestAnimationFrame(updateMargins);
    setTimeout(updateMargins, 150);
  }

  function onKeydown(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    if (t && ["INPUT", "SELECT", "TEXTAREA"].includes(t.tagName)) return;
    switch (e.key) {
      case "f": case "F": e.preventDefault(); toggleFullscreen(); break;
      case "Escape": if (fullscreen) toggleFullscreen(); break;
      case " ": if (nowPlaying) { e.preventDefault(); togglePause(); } break;
      case "ArrowLeft": if (nowPlaying) seek(-10); break;
      case "ArrowRight": if (nowPlaying) seek(10); break;
    }
  }

  onMount(async () => {
    try {
      await init(mpvConfig);
      unlisten = await observeProperties(OBSERVED, ({ name, data }) => {
        switch (name) {
          case "pause": paused = data as boolean; break;
          case "time-pos": position = (data as number | null) ?? 0; break;
          case "duration": duration = (data as number | null) ?? 0; break;
          case "volume": volume = (data as number | null) ?? volume; break;
        }
      });
      ready = true;
      requestAnimationFrame(updateMargins);
      window.addEventListener("resize", updateMargins);
      window.addEventListener("keydown", onKeydown);
      window.addEventListener("mousemove", revealControls);
    } catch (e) {
      initError = `mpv failed to start: ${e}`;
      error = initError;
      console.error(initError);
    }

    await loadSources();
    await Promise.all([loadGroups(), reload()]);
    if (sources.length === 0) showAdd = true;
  });

  onDestroy(() => {
    unlisten?.();
    window.removeEventListener("resize", updateMargins);
    window.removeEventListener("keydown", onKeydown);
    window.removeEventListener("mousemove", revealControls);
    clearTimeout(hideTimer);
    destroy().catch(() => {});
  });

  async function guard<T>(fn: () => Promise<T>): Promise<T | undefined> {
    error = "";
    try {
      return await fn();
    } catch (e) {
      error = String(e);
    }
  }

  const ref = (c: Channel) => ({
    url: c.url, name: c.name, group: c.group, tvg_logo: c.tvg_logo,
  });

  // ---- Sources ----
  async function loadSources() {
    sources = (await invoke<Source[]>("list_sources").catch(() => [])) ?? [];
  }

  async function addSource() {
    adding = true;
    await guard(async () => {
      const name =
        addName.trim() ||
        (addKind === "xtream" ? addHost.trim() : "Playlist");
      if (addKind === "m3u") {
        await invoke("add_m3u_source", { name, location: addLocation.trim() });
      } else {
        await invoke("add_xtream_source", {
          name,
          host: addHost.trim(),
          username: addUser.trim(),
          password: addPass,
        });
      }
      showAdd = false;
      addName = ""; addUser = ""; addPass = "";
      await loadSources();
      await loadGroups();
      await reload();
    });
    adding = false;
  }

  async function refreshSel() {
    if (sourceId == null) return;
    busy = true;
    await guard(async () => {
      await invoke("refresh_source", { id: sourceId });
      await loadSources();
      await loadGroups();
      await reload();
    });
    busy = false;
  }

  async function removeSel() {
    if (sourceId == null) return;
    busy = true;
    await guard(async () => {
      await invoke("remove_source", { id: sourceId });
      sourceId = null;
      await loadSources();
      await loadGroups();
      await reload();
    });
    busy = false;
  }

  // ---- Filtering / paging ----
  async function loadGroups() {
    groups =
      (await invoke<GroupCount[]>("list_groups", { sourceId }).catch(() => [])) ?? [];
  }

  async function reload() {
    if (view === "favorites") {
      favorites = (await invoke<Channel[]>("list_favorites").catch(() => [])) ?? [];
      return;
    }
    if (view === "recents") {
      recents = (await invoke<Channel[]>("list_recents", {}).catch(() => [])) ?? [];
      return;
    }
    offset = 0;
    const rows = await fetchPage(0);
    channels = rows;
    offset = rows.length;
    hasMore = rows.length === PAGE;
  }

  async function loadMore() {
    if (loading || !hasMore || view !== "channels") return;
    loading = true;
    const rows = await fetchPage(offset);
    channels = [...channels, ...rows];
    offset += rows.length;
    hasMore = rows.length === PAGE;
    loading = false;
  }

  async function fetchPage(off: number): Promise<Channel[]> {
    return (
      (await invoke<Channel[]>("search_channels", {
        query, group: group || null, sourceId, limit: PAGE, offset: off,
      }).catch((e) => { error = String(e); return []; })) ?? []
    );
  }

  let debounce: ReturnType<typeof setTimeout>;
  function onFilterChange() {
    clearTimeout(debounce);
    debounce = setTimeout(reload, 180);
  }
  function onSourceChange() { loadGroups(); reload(); }
  function setView(v: View) { view = v; reload(); }
  function onListScroll(e: Event) {
    const el = e.target as HTMLElement;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - 400) loadMore();
  }

  // ---- Favorites ----
  async function toggleFav(c: Channel) {
    const val = await invoke<boolean>("toggle_favorite", { channel: ref(c) }).catch(
      (e) => { error = String(e); return c.is_fav; }
    );
    channels = channels.map((x) => (x.url === c.url ? { ...x, is_fav: val } : x));
    recents = recents.map((x) => (x.url === c.url ? { ...x, is_fav: val } : x));
    if (!val) favorites = favorites.filter((x) => x.url !== c.url);
    else if (view === "favorites") favorites = favorites.map((x) => (x.url === c.url ? { ...x, is_fav: true } : x));
  }

  // ---- Playback ----
  async function playChannel(c: Channel) {
    if (!ready) {
      // Surface the original startup failure instead of a cryptic
      // "instance not found" from the plugin.
      error = initError || "player is still starting — try again in a moment";
      return;
    }
    nowPlaying = c;
    await guard(async () => {
      await command("loadfile", [c.url]);
      await setProperty("pause", false);
      await invoke("record_play", { channel: ref(c) });
    });
    if (view === "recents") reload();
  }
  const togglePause = () => guard(() => setProperty("pause", !paused));
  const stop = () => guard(async () => { await command("stop"); nowPlaying = null; });
  const seek = (s: number) => guard(() => command("seek", [String(s), "relative"]));
  const onVolume = (e: Event) =>
    guard(() => setProperty("volume", Number((e.target as HTMLInputElement).value)));

  const fmt = (s: number) =>
    !s || s < 0 ? "0:00" : `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
</script>

<div class="shell" class:immersive={fullscreen} class:hide-controls={fullscreen && !controlsVisible}>
  <aside class="sidebar" bind:this={sidebarEl}>
    <div class="head">
      <span class="brand">Vista TV</span>
      <button class="add-btn" onclick={() => (showAdd = !showAdd)} title="Add playlist / sign in">
        {showAdd ? "×" : "＋"}
      </button>
    </div>

    {#if showAdd}
      <div class="add">
        <div class="seg">
          <button class:sel={addKind === "m3u"} onclick={() => (addKind = "m3u")}>M3U</button>
          <button class:sel={addKind === "xtream"} onclick={() => (addKind = "xtream")}>Xtream login</button>
        </div>
        <input placeholder="Name (optional)" bind:value={addName} spellcheck="false" />
        {#if addKind === "m3u"}
          <input placeholder="M3U URL or file path" bind:value={addLocation} spellcheck="false" />
        {:else}
          <input placeholder="Host — http://server:port" bind:value={addHost} spellcheck="false" />
          <input placeholder="Username" bind:value={addUser} spellcheck="false" />
          <input placeholder="Password" type="password" bind:value={addPass} />
        {/if}
        <div class="row">
          <button class="primary" onclick={addSource} disabled={adding}>
            {adding ? "Adding…" : addKind === "xtream" ? "Sign in & add" : "Add playlist"}
          </button>
          <button onclick={() => (showAdd = false)}>Cancel</button>
        </div>
      </div>
    {/if}

    <div class="src-row">
      <select bind:value={sourceId} onchange={onSourceChange} class="src">
        <option value={null}>All sources</option>
        {#each sources as s}
          <option value={s.id}>{s.name} ({s.count})</option>
        {/each}
      </select>
      {#if sourceId != null}
        <button class="mini" onclick={refreshSel} disabled={busy} title="Refresh">⟳</button>
        <button class="mini" onclick={removeSel} disabled={busy} title="Remove">✕</button>
      {/if}
    </div>

    <div class="tabs">
      <button class:sel={view === "channels"} onclick={() => setView("channels")}>Channels</button>
      <button class:sel={view === "favorites"} onclick={() => setView("favorites")}>★ Favorites</button>
      <button class:sel={view === "recents"} onclick={() => setView("recents")}>Recents</button>
    </div>

    {#if view === "channels"}
      <input class="search" bind:value={query} oninput={onFilterChange} placeholder="Search channels…" spellcheck="false" />
      <select bind:value={group} onchange={onFilterChange} class="groups">
        <option value="">All groups</option>
        {#each groups as g}
          <option value={g.name}>{g.name} ({g.count})</option>
        {/each}
      </select>
    {/if}

    <div class="list" onscroll={onListScroll}>
      {#each items as c (c.url)}
        <div
          class="chan"
          class:active={nowPlaying?.url === c.url}
          onclick={() => playChannel(c)}
          onkeydown={(e) => e.key === "Enter" && playChannel(c)}
          role="button"
          tabindex="0"
          title={c.name}
        >
          {#if c.tvg_logo}
            <img src={c.tvg_logo} alt="" loading="lazy" onerror={(e) => ((e.target as HTMLImageElement).style.visibility = "hidden")} />
          {:else}
            <span class="nologo"></span>
          {/if}
          <span class="cname">{c.name}</span>
          <button
            class="star"
            class:on={c.is_fav}
            onclick={(e) => { e.stopPropagation(); toggleFav(c); }}
            title={c.is_fav ? "Remove favorite" : "Add favorite"}
          >{c.is_fav ? "★" : "☆"}</button>
        </div>
      {/each}

      {#if items.length === 0}
        <div class="empty">
          {#if view === "favorites"}No favorites yet — tap ☆ on a channel.
          {:else if view === "recents"}Nothing played yet.
          {:else if sources.length === 0}Add a playlist or sign in to get started.
          {:else}No channels match.{/if}
        </div>
      {/if}
    </div>
  </aside>

  <main class="stage-wrap">
    <div class="stage" ondblclick={toggleFullscreen} role="presentation">
      {#if !ready}
        <div class="hint">Starting mpv…</div>
      {:else if !nowPlaying}
        <div class="hint">Select a channel to start playback.</div>
      {:else if fullscreen && controlsVisible}
        <div class="fs-hint">Press <b>F</b> or <b>Esc</b> to exit fullscreen</div>
      {/if}
    </div>

    <footer class="transport" bind:this={transportEl}>
      <button onclick={() => seek(-10)} disabled={!nowPlaying}>⏪ 10s</button>
      <button onclick={togglePause} disabled={!nowPlaying}>{paused ? "▶" : "⏸"}</button>
      <button onclick={() => seek(10)} disabled={!nowPlaying}>10s ⏩</button>
      <button onclick={stop} disabled={!nowPlaying}>⏹</button>
      <span class="np">{nowPlaying?.name ?? ""}</span>
      <span class="time">{fmt(position)}{#if duration > 0} / {fmt(duration)}{/if}</span>
      <label class="vol">🔊
        <input type="range" min="0" max="130" value={volume} oninput={onVolume} disabled={!ready} />
      </label>
      <button class="fs" onclick={toggleFullscreen} title="Fullscreen (F)">
        {fullscreen ? "⤡" : "⛶"}
      </button>
    </footer>
  </main>

  {#if error}<p class="error">{error}</p>{/if}
</div>

<style>
  :global(html), :global(body) {
    background: transparent !important;
    margin: 0; height: 100%; overflow: hidden;
  }
  .shell {
    position: fixed; inset: 0; display: flex;
    font-family: Inter, system-ui, sans-serif; color: #eef0f4;
  }

  .sidebar {
    width: 340px; flex-shrink: 0;
    display: flex; flex-direction: column; gap: 0.5rem;
    padding: 0.75rem;
    background: rgba(14, 15, 20, 0.86);
    backdrop-filter: blur(18px);
    border-right: 1px solid rgba(255, 255, 255, 0.07);
  }
  .head { display: flex; align-items: center; justify-content: space-between; }
  .brand { font-weight: 700; font-size: 1.05rem; letter-spacing: -0.02em; }
  .add-btn { padding: 0.2rem 0.55rem; font-size: 1rem; line-height: 1; }

  input, select {
    padding: 0.45rem 0.6rem; border-radius: 8px;
    border: 1px solid #33343d; background: #191a20; color: inherit;
    font-size: 0.85rem; min-width: 0; width: 100%; box-sizing: border-box;
  }

  button {
    padding: 0.42rem 0.7rem; border-radius: 8px;
    border: 1px solid #3a3b45; background: #24252d; color: inherit;
    font-size: 0.83rem; cursor: pointer; transition: background 0.12s, border-color 0.12s;
  }
  button:hover:not(:disabled) { background: #2e2f39; border-color: #4a4b57; }
  button:disabled { opacity: 0.45; cursor: default; }
  button.primary { background: #3b6cf6; border-color: #3b6cf6; font-weight: 600; }
  button.primary:hover:not(:disabled) { background: #4d7bff; }

  .add {
    display: flex; flex-direction: column; gap: 0.4rem;
    padding: 0.6rem; border-radius: 10px;
    background: rgba(255, 255, 255, 0.04); border: 1px solid rgba(255,255,255,0.08);
  }
  .add .row { display: flex; gap: 0.4rem; }
  .add .row button { flex: 1; }
  .seg { display: flex; gap: 0.3rem; }
  .seg button { flex: 1; }
  .seg button.sel { background: #3b6cf6; border-color: #3b6cf6; }

  .src-row { display: flex; gap: 0.35rem; }
  .src { flex: 1; }
  .mini { padding: 0.42rem 0.55rem; width: auto; }

  .tabs { display: flex; gap: 0.3rem; }
  .tabs button { flex: 1; padding: 0.4rem 0.3rem; }
  .tabs button.sel { background: #2e2f39; border-color: #5a6cf6; color: #cdd6ff; }

  .list {
    flex: 1; overflow-y: auto; margin-top: 0.15rem;
    display: flex; flex-direction: column; gap: 2px;
  }
  .chan {
    display: flex; align-items: center; gap: 0.55rem;
    padding: 0.38rem 0.5rem; border: 1px solid transparent;
    background: transparent; border-radius: 8px; width: 100%; cursor: pointer;
  }
  .chan:hover { background: rgba(255, 255, 255, 0.06); }
  .chan.active { background: rgba(59, 108, 246, 0.22); border-color: rgba(59,108,246,0.5); }
  .chan img, .nologo {
    width: 26px; height: 26px; flex-shrink: 0; object-fit: contain;
    border-radius: 5px; background: rgba(255,255,255,0.06);
  }
  .cname {
    flex: 1; font-size: 0.85rem;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .star {
    padding: 0.1rem 0.3rem; border: none; background: transparent;
    color: #8a8b95; font-size: 1rem; line-height: 1;
  }
  .star:hover { background: transparent; color: #ffd257; }
  .star.on { color: #ffcf3f; }
  .empty { color: #8a8b95; font-size: 0.85rem; padding: 1rem 0.4rem; text-align: center; }

  .stage-wrap { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  .stage { flex: 1; display: flex; align-items: flex-start; justify-content: center; }
  .fs-hint {
    margin-top: 1rem; padding: 0.4rem 0.9rem; border-radius: 999px;
    background: rgba(20, 20, 26, 0.6); backdrop-filter: blur(8px);
    font-size: 0.8rem; color: #cfcfd8;
  }
  .fs-hint b { color: #fff; }
  .fs { width: auto; }

  /* Immersive fullscreen: hide the sidebar, float + auto-hide the transport. */
  .shell.immersive .sidebar { display: none; }
  .shell.immersive .transport {
    position: absolute; left: 0; right: 0; bottom: 0;
    transition: opacity 0.25s ease, transform 0.25s ease;
  }
  .shell.immersive.hide-controls .transport {
    opacity: 0; transform: translateY(100%); pointer-events: none;
  }
  .shell.immersive.hide-controls { cursor: none; }
  .hint {
    padding: 0.6rem 1rem; border-radius: 10px;
    background: rgba(20, 20, 26, 0.55); backdrop-filter: blur(8px);
    font-size: 0.9rem; color: #c7c7d0;
  }

  .transport {
    display: flex; align-items: center; gap: 0.5rem;
    padding: 0.55rem 0.8rem; background: rgba(16, 16, 20, 0.72);
    backdrop-filter: blur(14px); border-top: 1px solid rgba(255,255,255,0.06);
  }
  .transport button { width: auto; }
  .np {
    margin-left: 0.5rem; font-size: 0.85rem; color: #d7d7df;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 40ch;
  }
  .time { margin-left: auto; font-variant-numeric: tabular-nums; color: #b7b7c0; font-size: 0.85rem; }
  .vol { display: flex; align-items: center; gap: 0.35rem; font-size: 0.85rem; }
  .vol input { width: auto; }

  .error {
    position: absolute; bottom: 4rem; left: 50%; transform: translateX(-50%);
    margin: 0; color: #ffdada; background: rgba(180, 40, 40, 0.9);
    border-radius: 8px; padding: 0.5rem 0.9rem; font-size: 0.82rem; max-width: 60%;
  }
</style>
