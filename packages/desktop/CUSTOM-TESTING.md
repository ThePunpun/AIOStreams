# AIOStreams Custom — seek previews, revision 7

Windows x64 portable build; cached debrid only. Close the old app and extract this ZIP into a fresh folder. Keep its contents together. You can copy the old `data` folder while both apps are closed to preserve login/settings.

## One launch, default settings

Double-click **Start-Default.cmd** beside **AIOStreams-Custom.exe**. Enable previews in **Settings → Playback → Controls**. No PowerShell variants are needed. Switch videos inside this one launch.

1. **Same LOTR DV7 remux:** hover at a new position within five seconds of playback starting, hold until the image appears, and note the delay. Move the pointer away from the timeline and play untouched for ten minutes. Then check near playback, five minutes ahead/behind and fifteen minutes ahead/behind, holding each for five seconds. Repeat. Report colours, loading gaps and main-player buffering. For the whole-film coverage check, continue hands-off to 45 minutes total and then slowly sweep the whole timeline. Record gaps on this first sweep; repeated hovers can fill them.
2. **Fast sweep:** sweep rapidly on any file and confirm the thumbnail box stays at the same height. Leave the timeline hit area and confirm it hides immediately. The hover timestamp remains; the extra “Preview at” row is gone. Chapter/segment titles can still change.
3. **Same SDR episode:** play untouched for three minutes, then slowly sweep the entire timeline once. Record any “Loading preview…” positions, then repeat them. Check near playback and one/two minutes ahead/behind.
4. **Same DV P5 episode:** hover three new positions for five seconds each. Report natural colours, green/purple output, or “Preview unavailable”. Add P8 if available.
5. **Recovery:** seek the main player to a distant position, then hover while it is seeking/loading. After playback has shown its first frame, a main-player seek should not itself block hover previews. If a remote seek stalls, wait for recovery and try another position. Scrub new positions for eight–ten minutes on a short file if practical; previews must not permanently stop because of a demand-work limit. Cached images should remain usable during retries.
6. **Optional two-hour 1080p SDR movie:** let it fill while playing hands-off, then sweep for cache-size gaps. Note elapsed time. Switch episode/release and confirm old previews disappear.

The log records coverage every thirty seconds, including holes within ±5 and ±15 minutes of playback. Targets: the 23-minute SDR episode still fills within about three minutes; LOTR at 8-second spacing fills ±15 minutes in roughly six minutes and the whole film in roughly 45 minutes. These LOTR times are extrapolated from V6, not measured V7 guarantees. Keyframe spacing and provider/network behaviour affect them. Uncached startup positions necessarily show loading while their first capture runs.

## Collect once, after all tests

Close the app completely. In that same extracted folder, double-click **Collect-Preview-Logs.cmd**. Wait for **Created Preview-Logs.zip with … logs**, then press any key. Attach **Preview-Logs.zip**. You may rename it **v7-preview-logs.zip**; keep `.zip`.

**Do not collect between videos.** Each app launch creates one timestamped `seek-previews-*.log` in `data/logs`; every video tested in that launch goes into that log. The collector includes all dedicated preview logs present, including copied older logs. If you relaunch, collect once after the last launch to include them all. It replaces the ZIP each time, so collect after the final test.

If the collector fails, compress all `seek-previews-*.log` files from `data/logs`. Ordinary application logs are excluded because they can contain stream URLs.

## Changes to relay

- Sampling is **5 seconds for videos up to 60 minutes inclusive, 8 seconds above 60 minutes**. Native hover/background work and UI validation use the same grid and half-step lookup tolerance.
- Removed the extra “Preview at” row. The existing fixed image area, outline, black bars and instant hide remain.
- Both caches now allow **24 MiB of encoded image text and 10,801 images**, allocated as needed. V6's observed image sizes project to about 13 MiB for LOTR at 8 seconds and about 11 MiB for a two-hour SDR movie. This leaves headroom without choosing 32 MiB. These are separate native/UI limits, not a 24 MiB total-process RAM cap; decoded images and other playback memory are additional. Very long/high-complexity files can still reach the cap. Background stops rather than endlessly evicting its own work; direct hovers can replace older images.
- Background runs at up to 50% decoder duty when the main reader is idle/non-underrun and at least ten seconds are buffered; otherwise up to 25%. It pauses below ten seconds, with unknown buffer health, while paused/seeking/buffering, and while hovering. An idle reader is not proof of spare network bandwidth. This controls activity, not exact traffic.
- Removed the cumulative three-minute demand budget and total-capture shutoff. Ordinary open/seek/decoder failures use automatic retry with exponentially increasing delay up to thirty seconds. Ninety seconds of failed work triggers cooldown, not permanent session failure. Successful captures clear that failure window; a redundant playhead screenshot after reopening cannot mask a repeatedly failing far seek. Broken private decoders are reopened without resetting the main player. Explicitly unsupported formats/colour conversion still fail safely.
- Demand requires the current source and a first main frame, then remains allowed during main-player seeking/buffering. Background retains stricter health checks. The V6 log cannot prove the precise cause of its 57-second waiting period; V7 records the actual waiting reason, seeking/buffering/source flags and failure cooldown.
- Removed dormant neighbour prefetch and its state. Whole-file fill still chooses the nearest uncovered complete slot, two ahead for each one behind, recomputing coverage as playback advances.
- Cached UI hovers now display their existing image without requesting another native JPEG copy. Cached display events are counted in thirty-second coverage summaries instead of individually logged. Cold hover-to-image and first-image timings remain; long waits are no longer discarded after sixty seconds.
- Scale-first conversion, in-memory JPEG encoding, early decoder warm-up and ten-minute idle-close remain. No parallel downloaders or new aggressive decoder settings were added. Real Range fixtures check short and long sampling, HDR/HLG, aspect ratios, cancellation, main-seek independence and cleanup. Real P5 checks conversion or bounded safe fallback; natural colours still need visual confirmation on the tester's GPU.

Optional traffic check: compare equal five-minute LOTR hands-off runs with previews on/off. Resource Monitor shows live rates, not a cumulative transferred-byte total. Record rates or an actual traffic counter. Preview queue sizes are not transferred bytes.

Usenet preview support is still deferred. Experimental environment variants remain available for developer diagnostics, outside this test list.
