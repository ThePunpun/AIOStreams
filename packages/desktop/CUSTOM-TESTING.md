# AIOStreams Custom — V10 test conditions

Extract into a fresh folder with the old app closed. Keep ZIP contents together. You may copy the old `data` while both apps are closed to retain settings/logs; V9's mixed cost history is intentionally relearned. Enable previews in **Settings → Playback → Controls**. Use the .cmd launchers, one app copy at a time; fully close between launchers.

**Keep read-ahead 128 segments, Newshosting 39, Easynews 39, pipeline depth 1** for the main tests. Keep the two load-balanced/two backup provider arrangement and other settings unchanged. Use the same releases as V9. Avoid other downloads/streams if possible; record concurrent load. Start files from the same position, preferably zero, and record it. An empty preview session is not proof of a cold server/provider/disk cache.

Default Usenet/HTTP ceiling is **50% duty, 60 background seeks/min** with strict gates. Starts at 25%, not instantly 50%. Weak measured buffer/reader health pauses fill while demand remains available. Configured connection limits alone do not disable previews.

## Required launch — Start-Default.cmd

### 1. Usenet SDR, ten minutes

1. Play the **same Usenet SDR release** as V9, not the different Debrid SDR. In the server dashboard note settings and busy/active connections before playback, then near minutes **1, 3, 5, 10**. Record peak per provider, provider-limit errors and concurrent activity. Distinguish idle connected sockets from busy transfers if shown. Do not send credentials.
2. After the first main frame, play **ten minutes hands-off without main seeking**. Keep the pointer off the timeline. Record buffering with approximate wall-clock time, any nearby click/seek, and dashboard errors.
3. At ten minutes sweep the entire timeline **once**, then hold **10%, 30%, 50%, 70%, 90%** five seconds each. Note gaps/positions. Coverage before the sweep is the clean background result; the sweep adds demand work.
4. Move away **two minutes**, then repeat those positions. Note cache retention/speed.

### 2. Usenet real P5, ten minutes plus pause

5. Repeat steps 1–3 on the **same real DV P5 release**. This is a fill/main-health test; no full P7/P8 colour matrix is required again. Report any colour regression and the same dashboard samples/errors.
6. Move away and **pause three minutes**. Hover those five positions once; confirm main picture/time stayed paused. Resume one minute and record gaps, buffering and dashboard activity. Paused fill still needs healthy buffer/reader.
7. Reopen P5 once. Within about five seconds of its first frame, hold a new distant hover up to five seconds; estimate first-image delay. Leave and try another cold distant point. This separate session checks warm open without contaminating the earlier hands-off run. A 1.5-second first-hover target applies only if warm open had health/time to finish.

### 3. Debrid regressions

8. **LOTR:** same remux, **eight minutes hands-off**, then one sweep. Note spacing, holes, buffering. Reopen and hold **20, 40, 60, 80, 100, 120 minutes** five seconds each, then repeat for cached speed. Initial default spacing may be **39–40 seconds**; later learned sessions roughly **40–45** if costs support it. Spacing cannot change during a session. Eight minutes is a measurement target, not a promised deadline.
9. **4K HDR10 episode:** five minutes hands-off, sweep and three distant holds. **Debrid SDR:** three minutes hands-off, sweep and three holds. Note colours/gaps/buffering. Do not substitute P5 for the HDR10 comparison.
10. Cross named/unnamed chapters, check ellipsis/time/fixed height. On chapterless video, time only. Sweep, hold twenty seconds, exit/re-enter: instant hide, no held old image. Entry while unavailable should show time/chapter only. Failure mid-sweep may reserve transparent space until exit so the label does not jump.

### 4. HTTP and unavailable appearance

11. **HTTP addon:** same finite 1080p remux, Usenet indexers disabled as before. **Eight minutes hands-off**, sweep and five distant holds. Note gaps, slow hovers, buffering. Wait two minutes away, then try another cold point. V9's isolated 5.5-second seek has unknown cause.
12. **Unavailable appearance without a live addon:** close the app, launch **Start-Test-Unavailable.cmd**, and play a normal working video with previews enabled. Main playback should continue, but hovering must show **time/chapter only**, with no thumbnail, empty black box or error text. Sweep, hold twenty seconds, exit/re-enter and cross chapters. Toggle previews **off**, repeat the same hover positions and compare the tooltip appearance/height. Toggle **on** again: the test launcher still forces unavailable. Close and restart **Start-Default.cmd** with previews enabled; normal thumbnails must return. Record both appearance and restart result. If readily available, repeat on a chapterless video (time only); no live/nonseekable addon is needed.

No physical disconnect/reconnect repeat is required in this default test round. Kevin has already confirmed recovery. Default connection/retry options are unchanged and the developer failed-open recovery fixture passes. Recheck a physical outage only if testing the optional persistent-HTTP variant or a future transport/retry change. The injected unavailable test covers the terminal off-look from entry; it does not simulate a transient network outage or a failure partway through an already framed hover.

## Optional fallback or weaker-server comparison

If default buffers, hits provider limits or fails admission despite good playback, close and repeat the affected file with **Start-Usenet-Baseline.cmd** (25%, 30/min). For a matched pair use **Start-Usenet-Stage2.cmd** (50%, 60/min) for the other arm. Both disable learning/writing and use the same classic history/default grid. Keep settings/source/start/window identical, label runs and check grid/step equality in logs. Learning disabled still reads existing history.

**Start-Usenet-Demand-Only.cmd** disables Usenet speculation/warm open; demand remains. **Start-Demand-Only.cmd** disables Usenet and HTTP background; Debrid remains normal. These are fallback controls, not extra required launches.

Optional read-ahead study: **32 segments with both providers still 39 and pipeline 1**, fresh video session, ten-minute SDR/P5 run. Record health/dashboard/coverage; restore **128**. Only then, if useful, test **15 connections each with read-ahead still 128**; restore **39 each** afterward. Do not change both knobs together. Stop a weaker-setting trial if playback buffers/provider rejects. Read-ahead 32 does not necessarily prevent fill: segment size, bitrate, reader activity and load matter.

## Optional fast study — separate from this Usenet-focused round

Fast is opt-in. **Start-Comparison-Classic.cmd** / **Start-Comparison-Fast.cmd** match grid and disable learning/writing. Debrid only. Individual options: **Start-Comparison-Persistent-HTTP.cmd**, **Start-Comparison-Skip-Loop-Filter.cmd**, **Start-Comparison-MKV-No-Duration.cmd**. Each changes one option with otherwise classic comparison settings.

For promotion collect **≥20 actually cold displayed hovers per arm** on LOTR and HDR10 episode, hold until answered, counterbalance order and separate positions. Fresh sessions empty thumbnail caches but nearby keyframes/background can cover points; analyst must filter cached/opening events. Test components separately before choosing a default, check SDR colours/speed, and repeat outage recovery with persistent HTTP enabled. Keep fast opt-in unless repeatable improvement is ≥15% on both LOTR/4K, SDR does not regress and recovery passes. Short impressions alone do not settle it.

## Collect once after all chosen launches

Close the app; run **Collect-Preview-Logs.cmd once at the end**. Wait for “Created Preview-Logs.zip with … logs.” Do not delete logs between tests. Collector packages dedicated preview logs, retains originals, replaces ZIP. Up to forty launch logs retained; headers distinguish versions. Include black-screen/relaunch logs.

Send **Preview-Logs.zip + V10-ANALYST-BRIEF.md** to the analyst chat. The standalone brief includes all prior context, changes and this plan. Add observations using its checklist and mark skipped steps; the plan is not a claim of completion.
