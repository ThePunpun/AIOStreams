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
