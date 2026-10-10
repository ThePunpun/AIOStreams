# AIOStreams Desktop seek previews — self-contained V10 analyst brief

**Send this file with V10's Preview-Logs.zip. No earlier chat, V8/V9 relay, patch or handoff is required to understand the test.** Distinguish previous observations, implementation, planned tests and completed V10 results. At build time Kevin has not yet tested V10; fill the observation sheet after his run.

## 1. Project, scope and source anchors

Kevin tests a Windows x64 portable custom desktop player. This is the AIOStreams desktop/Jellyfin-web player, not his Nuvio installation or a change to the streaming server. A single independent private libmpv preview decoder reads the actual selected stream/file; it does not seek the main decoder. Preview settings are in Playback → Controls. Source metadata distinguishes cached Debrid, native Usenet and HTTP addon links. Live/infinite, P2P, uncached Debrid, external playback and unknown/unsupported sources stay off.

- Fork: https://github.com/ThePunpun/AIOStreams ; upstream Viren070/AIOStreams.
- Feature branch: `refs/heads/desktop-seek-previews`; draft fork PR: https://github.com/ThePunpun/AIOStreams/pull/15 .
- Delivered V9 SHA: `564e526a42b497b7b5ccf84fee62b72d3b9e74ce`; V9 tree: `54a2bdc5f78eeed2bc0e99de1fc3ba5871896fff`.
- V10 tag: `desktop-seek-previews-v10`; download: https://github.com/ThePunpun/AIOStreams/releases/download/desktop-seek-previews-v10/AIOStreams-Custom-Windows-x64.zip .
- V10 exact SHA is in each log's `sha=` header; use only `preview_revision=10` for this comparison. The delivered standalone brief appends the final build/package audit.
- One amended feature commit, same original parent `147773cb80d5fd38a1dae7924b6b610abf1d92a9`, per Kevin's clean-PR rule. Revisions are sibling amendments. Exact revision diff is `git diff --binary --full-index V9_SHA V10_SHA`, not the PR diff or three-dot compare.
- Server branch `punpun-custom` and deployments are separate and untouched. GitHub Actions must be restored to **Disable actions** after building.
- Repository source: `packages/desktop/core/src/{preview_policy.rs,previews.rs,bridge.rs}`; `packages/desktop/app/src/{main.rs,logging.rs}`; UI `packages/jellyfin-web/src/{components/player-controls.tsx,lib/playback/preview-policy.ts,lib/hosts/shell/bridge.ts,lib/hosts/shell/previews.ts}`. Tests: core policy/worker tests, `packages/jellyfin-web/test/preview-policy.test.ts`, `packages/desktop/scripts/check-previews.py`, existing `check-p5.py`. Packaging: `.github/workflows/desktop-custom.yml`, `packages/desktop/scripts/Start-*.cmd`, `CUSTOM-TESTING.md`, `V10-CHANGES.md`, this brief.

Kevin's media backend is an Oracle ARM/Ubuntu Docker stack. Separate AIOStreams configurations use TorBox Debrid and native NNTP Usenet. Primary load-balanced providers are Newshosting/Easynews; backups Tweaknews/NewsDemon. The decoder reaches a playable HTTP stream; it does not own or see the server's NNTP pool.

**Main V10 conditions:** read-ahead **128 segments**; Newshosting/Easynews **39 configured connections each**; pipeline **1**. Keep backups/load-balancing and other settings fixed. These are tester-declared configured settings, not measured active connections. Concurrent background load and pool utilization were not captured in V9. Do not infer spare capacity merely from 39+39.

## 2. Standing product constraints and history

Use one private decoder, no parallel-worker feature. Finite timeline up to 24 hours; no live work. Generate matching frames from the selected file, freeze spacing per session, fill contiguous coverage without deliberate stride gaps. Timestamp follows the pointer even when a thumbnail covers a wider interval. Each native/UI cache is bounded to 600 images and 24 MiB encoded data; this is not a total process RAM guarantee. Native queue growth is not transferred-byte accounting.

No held old image, exit grace or “Preview at” row. Hide immediately on exit. Chapter names/numbered fallback/ellipsis were already good; no general UI redesign. Preserve working SDR/HDR/DV paths: scale-first tone mapping, in-memory JPEG, real P5 conversion through libplacebo when supported, safe bounded unavailable otherwise. Decoder colour evidence overrides misleading source hints; a good P7/P8 base is not proof of P5 conversion. Temporary network failure must retry/recover; unsupported paths are terminal. The explicitly marked unavailable test launcher is an injected terminal state used only for appearance testing.

Earlier lessons: aborting paid demand seeks could leave a rapidly moving pointer with no frames; an alternating background stride caused SDR coverage holes; a fixed byte budget blocked long fills; cumulative demux queue growth was wrongly interpreted as traffic. V10 does not reintroduce those behaviors.

## 3. What Kevin actually did in V9

Inputs fully ingested: `Preview-Logs(3).zip` (four launch logs, valid CRC), `Pasted markdown(1).md` (analyst V9→V10 relay), the full V9 developer handoff, media-stack reference and GitHub rules. All four logs identify the delivered V9 SHA/revision.

Launches dated October 9, 2026: normal 15:21, Usenet demand-only 16:11, classic comparison 16:19, fast comparison 16:20 (timestamps as logged; do not assume timezone conversion). Normal run included two substantial LOTR sessions, Debrid SDR/HDR10, real Usenet SDR/P5 background, brief Usenet P7/P8 demand hovers and HTTP demand. Some brief empty sessions are present; they are not evidence of testing more files.

Kevin reported:
- Debrid HDR10 episode filled the whole video in about five minutes, faster than LOTR.
- Usenet SDR is **a different release** from Debrid SDR. After 3–4 minutes it had roughly the played length cached; after moving main playback to ten minutes, thumbnails later extended ahead with little behind.
- Usenet P5/P7/P8 colours looked good. First hover was slower; he hovered within five seconds of playback. P5 after about five minutes had roughly 12–14 minutes cached each side, slower than LOTR. A full repeat P7/P8/P5 colour matrix is not requested.
- He deliberately ran the paused test on LOTR; logs also contain pauses/fill on Usenet SDR and P5.
- Usenet SDR/P5 cached images were still present after two minutes.
- HTTP addon-only 1080p remux hovers were generally fast. One random hover after a hands-off period took 5–6 seconds, then normal again.
- Classic/fast LOTR hovers “seemed same” visually, colours same; he believed logs favored fast.
- **He skipped physical disconnect/reconnect in the submitted V9 run**, doing only off/on reset. In the latest steering Kevin confirms an earlier physical network-drop test passed and declines a repeat unless related behavior changes. Do not relabel earlier evidence as a V9/V10 physical run; no default V10 repeat is required. Default connection/retry options remain unchanged and failed-open recovery has a developer fixture.

A later message accidentally relayed **V8-only** advice (“Usenet demand-only evidence”, earlier small fast deltas). Kevin explicitly withdrew that as guidance and authorized approximately 50–60% Usenet duty. Developer chose **50% maximum /60 attempts per minute**. Do not mix the withdrawn V8 numbers with this V9 evidence.

## 4. V9 measured baseline and limits of inference

Figures below are the V9 analyst's rounded figures, checked against the supplied raw log populations. Independent medians differ by a few milliseconds with inclusion/filter choices; compare equivalent issued cold work, exclude opening/cached frames, and report sample counts. Do not treat rounded medians as exact fixtures.

| Source/session | Duration; grid | Open | Issued seek/hover evidence | Result before end |
|---|---|---|---|---|
| Debrid LOTR #1 `ce4463fd…` | 13,691.758s; 267/51.281s | 1.58s | BG median ≈1.02s; ≈0.69/s steady | 80.5% at ≈338s; 216 images, not full |
| LOTR #2 `613fd837…` | same; 249/54.99s | 1.76s | BG ≈1.00s, ≈0.62/s overall | 92.8% at ≈361s, incl. 3-min pause |
| Debrid SDR `2cfd2f35…` | 1,343s; 269/5s | 0.31s | BG ≈0.20s | full at ≈212s; 231 images cover 269 slots |
| Debrid HDR10 4K `223dcfb0…` | 1,824s; 267/6.83s | 0.33s | BG ≈0.66s, ≈0.97/s | full at ≈302s, buffer 41–59s |
| Usenet SDR `d84996e3…` | 1,420s; 285/5s | 2.60s | BG ≈0.232s, ≈0.49/s (30/min binding) | 40.0% at ≈262s; main buffer ≈130–190s |
| Usenet P8 `d9a179f6…` | 9,759s; 320/30.5s | 4.38s | 5 demand samples ≈0.576s | ≈18s hover-only session |
| Usenet P7 `90e1a8f9…` | 6,920s; 415/16.7s | 2.90s | 5 demand samples ≈0.682s | ≈15s hover-only session |
| Usenet real P5 `b11600f2…` | 8,700s; 351/24.79s | 2.67s; reopen 1.42s | BG ≈0.70s, ≈0.32/s (.25 duty binding) | 30.8% at ≈381s; buffer ≈67–95s |
| HTTP `b0b0e830…` | 3,107s; 600/5.18s | 1.45s | 20 demand median 473.5ms; 14 cold displayed median ≈555ms | 21 images; zero background |
| LOTR classic `a35930e3…` | 240/57.049s; cost1.019 | separate short session | demand median1,380ms n9; cold hover1,806ms n6 | zero background during comparison |
| LOTR fast `0e4aed38…` | **same**240/57.049s | separate short session | demand median489ms n7; cold hover639ms n6 | short, later arm; no promotion proof |

More baseline evidence:
- Normal Debrid duty rose .50→.65→.80 with no step-down observed. Main buffers stayed healthy. Estimated busy fraction across fills about66–70%; this informed .65 expected grid duty, not a guaranteed utilization.
- LOTR learned cost drifted .900→.965→1.019, grid267→249→240. Persistence worked but five demand samples from Usenet P8 affected the shared class used by P7/P5. V10 separates populations and requires ten.
- Wide-file batching reduced reverse jumps (4K about13%). SDR had about41% backward jumps, roughly572ms backward vs205ms forward; V10 applies batches to SDR too.
- LOTR pause added123 images in183s (≈.67/s), buffer roughly15.5s unchanged, resume without unexpected buffering.
- Five `playback_buffering=true` events in the ZIP were **all `buffering_cause=seek`**, no observed genuine underrun. Usenet's substantive background runs had zero logged main-buffering intervals. Minimum buffer during Usenet background about67.8s. Some empty sessions should not inflate “number of tested files”.
- P5 one `playback-busy` background abort closed/reopened preview decoder; not a terminal failure. ±5-minute holes reached zero around2.5min and ±15 around5min.
- No `open_failed` or P5-filter failures in these V9 logs. **Provider-limit errors were not instrumented**, so absence in preview logs is not proof of no server errors.
- HTTP bucket363 issued seek took **5,533ms**. Pointer movement made old hover metric log≈1,553ms and hid most wait. Decoder snapshot showed input underrun; **root cause remains unknown**, not proven network/idle/decode. First hover after idle was normal. Five slow LOTR background seeks also occurred, mostly early.
- V9 rapid stable/rate-cap flapping was noisy; printed general duty level could be .8 although actual Usenet duty stayed .25.
- Classic/fast comparison was small, short, same-grid and fixed-order. Large V9 deltas are a reason to test, not proof of repeatability or recovery.

## 5. V10 implementation and decision record

# Seek previews V10 — changes from delivered V9

Baseline: `564e526a42b497b7b5ccf84fee62b72d3b9e74ce`, preview revision 9. V10 uses the immutable tag `desktop-seek-previews-v10`; its log header records the exact build SHA. The separate `v9-to-v10.patch` is the two-tree diff, not the whole PR patch.

## Changes

- **Usenet stage 2 is the default:** maximum duty 0.50, rolling cap 60 background seek attempts/min. Starts at 0.25, then 0.35 and 0.50 after successive 30-second stable windows following admission. Debrid retains its separate 0.50/0.65/0.80 ladder.
- **HTTP background fill:** finite, fully seekable HTTP files fill under the same strict controller as Usenet, ceiling 0.50 and cap 60/min. HTTP opens speculatively only after admission; hover demand can open earlier.
- **Health gates:** complete 60-second window with at least 30 seconds buffered throughout, current reader idle, no main buffering/input underrun within 300 seconds. Background aborts below 20 seconds buffered, on reader activity, main seek/buffering, or pointer demand. Unexpected underrun steps down and caps the session; health must recover again. Deliberate main seeking is classified separately but interrupts speculation and resets conservative admission.
- **Configurable ceilings/kill switches:** `AIOSTREAMS_PREVIEW_USENET_MAX_DUTY` and `AIOSTREAMS_PREVIEW_HTTP_MAX_DUTY` accept finite 0.25–0.50; invalid values use 0.50. Supported levels are 0.25, 0.35, 0.50, so intermediate ceilings round down. At configured 0.25 the seek cap is 30/min, otherwise 60/min. Background-off variables retain demand previews. Launchers clear inherited experiment variables.
- **Source-aware grids:** expected duty 0.65 Debrid, 0.40 Usenet/HTTP. Budget = `round(480 × expected duty / cost)`, clamp 240–600; spacing ≥5 seconds and frozen per session. Example: 13,692-second LOTR at 1.02-second Debrid cost →306 points/44.746 seconds; 8,700-second Usenet P5 at 0.684 →281/30.961. These are examples, not V10 measurements.
- **Learning hygiene:** ≥10 valid cold samples, latest 31; separate source/range/width, decoder variant, background versus demand. Full-mode trains only on background; demand-only trains on demand. Opening frames, buffered decoder seeks and failures do not train. Version-1 mixed history is ignored; version-2 is rebuilt without removing settings/logs. The 25% fallback retains the same grid formula for matched comparisons.
- **Forward batches for SDR:** existing four-point frontier now serves every class, covers both sides and revisits holes after movement/eviction. No alternate-slot stride.
- **Issued demand seeks finish:** movement no longer aborts an already issued demand seek; its result is cached before the newest request follows. Background still yields. Removes V9's early demand-cancellation experiment.
- **Stall metrics:** `demand_stall_ms` follows the first unanswered settled request until an image is displayed/ready or pointer exit. Bucket movement does not reset it. `slow_demand_seeks` counts issued demand captures >2 seconds, including failures. Existing per-bucket hover timing remains.
- **Usenet warm open:** full-mode may open the one decoder after a main frame, idle reader and ≥20 seconds buffer, before the 60-second scan gate. Includes SDR because its V9 open was also expensive. Demand-only does not warm-open.
- **Unavailable test launcher:** `Start-Test-Unavailable.cmd` sets `AIOSTREAMS_PREVIEW_TEST_UNAVAILABLE=on`, forces the existing terminal status (`reason=test-unavailable`), suppresses preview decoder work/history learning, and leaves main playback running. Logs explicitly mark the injected state. Every normal/fallback/comparison launcher clears the flag. This tests appearance without a live addon; it is not evidence that a real source is unsupported.
- **Unavailable appearance:** entry during unavailable shows time/chapter only. Geometry freezes for that pointer run. Failure during an already framed sweep leaves transparent reserved space until exit; next entry uses the smaller off-look. Transient failures keep retrying; no stale held image.
- **Diagnostics:** revision 10 header; named heuristic thresholds; effective decoder variant; cost sample/population; duty/ceiling/reason; gate failure; approximate rate-limit wait. Routine cap flapping is aggregated, health changes and periodic raw reasons retained. Server connections/read-ahead/pipeline are explicitly unavailable.
- **Packaging/testing:** normal, demand-only, matched 25%/50% Usenet, classic/fast, and three individual fast-option launchers. ZIP includes this note, guide, self-contained analyst brief. Native fixture exercises actual HTTP/Usenet background admission after 60 seconds while the main stays paused, plus stop cleanup.

## Decisions and deviations

Kevin corrected a later relay from a chat with only V8 data. Its demand-only Usenet claim and earlier comparison numbers do not describe the attached V9 logs. V9 actually had SDR and P5 background fill without logged unexpected main buffering. Kevin authorized the increase. We chose **50% duty /60 seeks per minute**, not 60% duty.

Fast-seek stays **opt-in for Debrid**, despite the original V9 relay's proposed default. Actual V9 LOTR favored fast markedly, but each arm had six displayed cold hovers, short unequal sessions and fixed order. 4K/SDR and physical outage behavior were unestablished. Persistent HTTP, skip-loop-filter and MKV duration can be tested separately. These options do not apply to Usenet/HTTP through the launchers.

480-second target, 20% promotion tolerance, 25% drop threshold, stability and backoff are named/logged **heuristics**, not proven optimal. Duty describes work/rest scheduling, not bandwidth or NNTP utilization. The client cannot establish provider headroom; dashboard observations are needed.

No connection-count/admin integration, parallel decoder, live/P2P fill or unrelated server changes. Chapters, scale-first colour conversion, safe P5 fallback, one worker, instant hide, 600-image/24-MiB encoded cache limits and bounded unsupported handling remain.

## Validation scope

Local Rust/core/UI checks and authenticated Range fixtures cover seeks, retry recovery, demand completion and real conservative admission/cleanup. CI checks the Windows app, all Range/colour fixtures, opt-in options and real P5 conversion **or safe bounded unavailability**. A safe-unavailable pass does not establish colour correctness. The delivered standalone analyst brief adds exact final CI/source/package results.

Real V10 Usenet/provider behavior and visual checks await Kevin's test. No user V10 result exists at build time. Kevin confirms prior physical network-drop recovery works; no repeat is required for the default launcher because default connection/retry options are unchanged. Failed-open retry/recovery remains covered by the authenticated fixture. Physical recovery is still a promotion check for the optional persistent-HTTP variant.

## 6. How to analyze V10 logs

**First inventory launches/headers and sessions.** Check revision, SHA, launcher variant, background booleans, session source kind/mode, ceiling/cap and effective decoder variant. Copied data may include older logs. Do not pool revisions or compare unsupported/empty sessions as successful tests.

- `grid_budget_slots` is formula budget; `grid_slots`/`total_buckets` is actual duration/step count. The five-second floor can make actual points fewer than budget. `grid_cost_source`, `grid_samples`, `grid_class`, `grid_expected_duty`, `step_ms` show the frozen choice. Source/range/width classes can still span multiple files/DV profiles; this is not file-specific benchmarking.
- V10 history v2 excludes V9 mixed history. Keeping data preserves settings/logs, not immediate old learned spacing. Normal learning is on; comparison launchers freeze training/writes but can read matching v2 history. Only later sessions use new samples.
- Defaults: Debrid HDR/wide cost .9, SDR .35; Usenet SDR .3 and other .75. Debrid expected .65: LOTR starts≈347 budget points/39.459s, later306/≈44.745s for measured≈1.02. Usenet expected .4: P5 default256/≈33.985s, later281/30.961 for .684. HTTP SDR default budget549 (not600), with ≈5.66s on the3,107s remux. Duration-rounded examples are not V10 results.
- **Admission is separate from ceiling:** background cannot scan just because 50% is configured. Window/buffer/reader/recent-buffering gates must pass. The first admitted level is .25; promotion clocks begin after admission, not at session creation. Reader-idle is current admission/ongoing-work protection; the entire 60s buffer window does not require continuously idle reader.
- `background_duty` is current permitted duty, `duty_level` controller level, `duty_ceiling` supported effective ceiling, `duty_reason` reason. `background_gate_fail` explains window/buffer/reader/recent-buffering. `background_state=rate-governed` aggregates cap cycling; periodic raw reason remains. `rate_limit_wait_ms` approximates sampled cap-blocked time, not precise network time.
- Rolling cap counts attempts, including failed/aborted background captures; it excludes demand and decoder opening. Duty governs speculative work/rest, not the total process, on-demand rate or bandwidth. In-flight polling/cancellation is bounded, not instantaneous at zero milliseconds.
- Whole-cache fill requires **covered_buckets==total_buckets /100%** before the sweep. Image count can differ because a keyframe interval covers multiple slots. Inspect both ±5/±15-minute holes and whole-file coverage. Early-end sessions yield partial coverage, not measured full-fill time. Subtract first-frame/admission delays consistently and label timing origin.
- `open_frame=true` has no issued seek. Exclude from cold seek medians. Filter cached displays and buffered decoder work; report n, median and tails. `seek_ms` is combined seek-to-frame wait; separate network/decode phases are unavailable. Capture/JPEG timing is separate.
- `hover_to_image_ms` remains per-bucket UI time to displayed cold image. `demand_stall_ms` spans moved buckets from first unanswered settled request, resets on matching answer/ready or exit. `ui=demand-ready` means usable image reached an off-look run but was **not displayed**; don't count as hover-to-image. `slow_demand_seeks` counts capture work >2s, excludes decoder-open and UI settle; use open/first-hover logs too.
- A completed old seek after pointer movement may populate cache without matching the current target. It is not stale image display. A new target follows; exit hides UI but paid work may finish. Background can be cancelled for pointer/main-health.
- `buffering_cause=seek` distinguishes deliberate click/seek from unexpected underrun; startup also separate. Input underrun can be observed without `playback_buffering=true`. Any input underrun resets the conservative recovery clock. Health attribution is heuristic: compare wall-clock/main events and tester observations, not simply a count of all buffering.
- 24MiB native/UI limits are encoded cache bounds. `demux_queue_growth_total_bytes` is queue-storage growth, not HTTP traffic. `network_bytes`, `network_wait_ms`, `decode_ms` are unavailable; do not calculate actual provider throughput/connections from them.
- `provider_connections`, `server_read_ahead`, `pipeline_depth` are explicitly unavailable. Manual dashboard samples are required. Client does not collect admin credentials or raw authenticated stream URLs in dedicated logs. Report general server/provider errors without secrets.
- Off-look does not disable retries. Unsupported reasons are session-terminal, transient retry-backoff recovers. Fixed geometry can leave a transparent spacer until pointer exit; image hidden in a recovered off-look run should appear on re-entry. Do not call that permanent disablement without testing re-entry.
- Colour conversion/fallback logs establish chosen path, not visual quality. Kevin's observation is needed for colours. A synthetic Range fixture represents the client protocol; it cannot prove NNTP/provider behavior.

## 7. Exact V10 test plan

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

## 8. Observation sheet — Kevin fills after V10

These rows are **planned, not completed**. Mark done/skipped/failed. Main server settings prefilled from Kevin; edit only to describe deliberate labelled trials.

| Item | Record |
|---|---|
| Build/launchers | V10 SHA/header; filenames used; launch order |
| Main setup | read-ahead128; Newshosting39; Easynews39; pipeline1; backups/load balance unchanged |
| Concurrent activity | none, or other streams/downloads with approximate timing |
| Usenet SDR | release/source; start position; 10-min untouched window done/skipped; gaps/locations; buffering with times; 2-min cache retention |
| Usenet P5 | same fields; pause/resume result; separate reopen first-hover estimate; colour regression if any |
| Dashboard | connected vs busy definition; per-provider samples before/1/3/5/10min; peak busy; provider-limit/error text/time |
| Debrid LOTR | 8-min untouched result; first/later session; apparent spacing; holes; cold/cached delay |
| Debrid HDR10 /SDR | 5/3-min results, colours, gaps, buffering |
| HTTP | 8-min result; slow-hover/stall times; source was finite HTTP addon |
| Recovery | prior physical test confirmed by Kevin; no default repeat required; optional persistent-HTTP outage result if that variant was tested |
| Forced unavailable | launcher; main continued playing; time/chapter-only vs previews off; no black/error box; normal thumbnails restored after default restart; chapterless check done/skipped |
| Tooltip | chapters, no chapter, fixed height, immediate hide, off-look/re-entry behavior |
| Optional baseline/Stage2 | source/settings/start/windows matched? grid/step same? observations in each |
| Optional weak settings | changed one knob? values/time; source/window; health/provider/gaps; restored128/39/39/1 |
| Optional fast/component | launcher/order, file, ≥20 usable cold samples? SDR/recovery results; omissions |
| Relaunches/failures | black screen or crash timing and recovery; retain all launch logs |
| Log collection | once at end; all selected launch logs included |

## 9. Requested analyst output after logs arrive

1. Inventory exact launches/files and distinguish background windows from hover/pause/seek periods. Confirm tests actually completed.
2. Compare V9→V10 per equivalent source/release and grid. Report coverage over time, fill time only if observed, local holes, first-open/cold/cached hover medians/tails/n, successful/failed/aborted work and rate/duty gates.
3. Judge Usenet50%/60-min default using **main buffering/input underrun + manual provider errors/load**, not healthy buffer alone. Decide retain50, reduce to25/35 or do another controlled run. Do not suggest >50 based solely on absence of preview-log errors.
4. Explain shallow read-ahead/low-connection cases using measured gates; keep demand available when background lacks health. A failure to fill can be the intended protection. Segment count is not seconds/bytes; avoid universal thresholds based on32/128.
5. Judge new HTTP fill and regressions, investigate stalls without guessing network/decode split. Check paid-demand completion and new timer consistency.
6. Keep fast opt-in until adequately sampled/counterbalanced per-file, SDR and outage/component evidence supports a change; do not promote from the small V9 comparison alone.
7. Return a **self-contained next-version relay**: observations, measured evidence, unknowns, prioritized concrete changes, heuristics to tune, acceptance targets/test conditions. Explicitly state deviations and what was not tested. Kevin should not need to send every prior message again.

Measurement goals from the original V9 analyst: LOTR≈40–45s learned/full≤8min; HDR10≤5min; SDR≤2.5min; Usenet SDR≤6min/P5≤10min with zero unexpected main buffering/provider-limit errors; HTTP approximately≤10min. They are goals, not hard guarantees or evidence of completion. Grid density, opening/admission time, load and seeks affect comparisons. Safety/retention/recovery matter alongside speed.
