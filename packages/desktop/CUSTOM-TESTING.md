# AIOStreams Custom — seek previews, revision 5

Windows x64 portable test build. Extract the ZIP to a new folder, then open the inner folder containing **AIOStreams-Custom.exe**. Close the previous custom build first. You can copy its `data` folder here while both apps are closed to keep your login/settings. Keep `.portable` beside the executable. Seek previews must be enabled in **Settings → Playback → Controls**.

## Test the default first

**The default now uses scale-first.** There is no need to run a separate scale-first trial.

1. **Same LOTR DV7 remux:** record the starting playback time. Hover an uncached position soon after playback starts. Hold six distant, previously unvisited positions for five seconds each, then repeat them to check cached display speed. Note colours, startup delay and later cold-hover delay separately.
2. Hold an uncached position for 20 seconds. Move just above/below the thin visible bar, then leave and return within 0.3 seconds. The preview should stay visible. Leaving for more than 0.4 seconds should hide it. Sweep rapidly, stop at a new point, then seek the main player and hover again. Check that the final hover wins and old images do not stick.
3. **LOTR: five minutes playing with the pointer away from the timeline.** Then hover near the current playhead and a minute ahead/behind. Note extra playback buffering, CPU load and whether these positions are cached. This is the missing long background test.
4. **Same SDR episode: three minutes playing with the pointer away.** Then hover near the playhead, one/two minutes ahead and behind, and repeat those positions. Note cache coverage and any added buffering. Background spacing is now at least 10 seconds; direct hovers keep their finer adaptive spacing.
5. **Same DV profile 5 episode:** hold three new positions for five seconds each. Check for natural colours, especially skin tones; green/purple output is a failure. V5 automatically attempts libplacebo reshaping on identified P5 sources. If Vulkan/filter startup fails, “Preview unavailable” is the supported fallback. Report either result and the actual DV profile when known.
6. Check a 4:3/wide file, episode switching, preview toggle persistence and ordinary seeking. The preview contains the entire picture in its consistent 16:9 frame, with bars as needed.
7. Close the app completely after the test. This finishes its session summaries.

Record the file/release, DV profile if known, approximate test time and visual symptoms in your message. Logs cannot prove colour accuracy.

## Send all logs together

Each app launch creates its **own** file in `data/logs`, for example `seek-previews-2026-10-07_18-42-10-1234.log`. All videos tested during that launch append to that file. Restarting creates another file; closing does not overwrite it. The newest 40 preview logs are kept.

After closing the app, double-click **Collect-Preview-Logs.cmd** beside the executable. It creates **Preview-Logs.zip** in that same folder. Attach that ZIP. It includes every dedicated preview log present, including the three trial launches if you run them. Running the collector again replaces the ZIP with a fresh collection. Ordinary application logs are excluded because they can contain stream URLs.

If the collector cannot run, select **all** `seek-previews-*.log` files in `data/logs`, right-click and compress them to a ZIP. Send the ZIP after finishing and closing the app. A copy sent earlier will not gain later test results.

## Optional colour comparison

Only run these after the default tests. Use the **same LOTR release, starting playback position and six distant hover positions** in each launch. Compare natural colours and the displayed sampled timestamp, then close the app fully before changing variants. Every launch starts a fresh preview cache.

Open the inner executable folder in File Explorer. Click the address bar, type **powershell**, and press Enter. Administrator rights are unnecessary. The prompt must show your executable folder, **not `C:\Windows\System32`**. `Test-Path .\AIOStreams-Custom.exe` should return `True`.

Paste each block as two separate lines.

Default / scale-first:

```powershell
Remove-Item Env:AIOSTREAMS_PREVIEW_TEST_VARIANT -ErrorAction SilentlyContinue
.\AIOStreams-Custom.exe
```

Old colour pipeline for comparison:

```powershell
$env:AIOSTREAMS_PREVIEW_TEST_VARIANT = 'tonemap-first'
.\AIOStreams-Custom.exe
```

Zscale-first:

```powershell
$env:AIOSTREAMS_PREVIEW_TEST_VARIANT = 'zscale-first'
.\AIOStreams-Custom.exe
```

Close the app after each block. Collect the logs once after all three launches. To return to the default, run the first block again. The environment setting affects only apps launched from that PowerShell window; removing it deletes no files. Closing the PowerShell window also clears its temporary setting.

Optional later LOTR speed trials use **one** variant at a time, with the same default-test positions:

| Variant | Change |
| --- | --- |
| `threads-1` | One software decoding thread instead of two |
| `persistent-http` | Request FFmpeg HTTP connection reuse; the server determines its effect |
| `dv-libplacebo` | Force the experimental libplacebo filter; identified P5 already uses it automatically |

`scale-first` remains an alias of the default HDR filter. `baseline` is an alias of `tonemap-first`, the older full-size tone-map path. Do not combine variants.

## Changes and diagnostic limits

- HDR filters are configured before loading when media metadata or the selected main track identifies the colour path. Compatible DV base layers skip the former two-stage preparation. Unknown metadata retains the detection fallback.
- Thumbnails use `screenshot-raw` and JPEG encoding in memory, eliminating temporary JPEG file open/read/delete stalls.
- Background work requires active, healthy playback and either 10 seconds buffered or an idle demux reader with at least 3 seconds buffered. Reader idle alone does not guarantee a full buffer.
- Speculative work rests at least three times its elapsed capture time: about 25% duty at most. Its estimated input allowance is 16 MiB at startup plus 20% of estimated bytes played. Buffering increases the rest period. Direct hover requests take priority.
- **This is an estimated input budget, not an exact download cap.** mpv exposes packet queue sizes and estimated input rate, not a cumulative transferred-byte counter. Logs say `background_estimated_bytes` and `network_bytes=unavailable`. Resource Monitor can compare whole-app traffic with previews on/off.
- Background searches four positions ahead for each one behind, on a coarser grid for short videos. A resting hover may prefetch one neighbour when median seek time is below 1,000 ms and buffer/budgets allow it. That neighbour follows the last hover direction.
- New settled hover targets may interrupt a demand seek before half its measured typical duration. Only one such interruption is allowed before a completed demand image, preventing repeated movement from starving long-GOP files. Later seeks finish and cache.
- Cached/cold display timing is measured separately from the 75 ms remote-request settle. The first image since first hover is logged. Variant and colour hint are logged from session start; decoder open records the confirmed path.
- P5 records `p5_filter=ok|failed:<reason>`, filter usability, output dimensions and Vulkan readiness. `ok` establishes a usable SDR output, **not verified metadata delivery or colour accuracy**. Those remain explicitly unverified until a real visual comparison. Unsupported conversion leaves previews unavailable without changing main playback.
- Cache limits remain 400 images / 8 MiB encoded in each cache. Decoder work limits remain 180 seconds overall, 90 seconds speculative, with 1,200 completed captures and a separate failed-work ceiling. Cached images remain usable after limits are reached.

Cached debrid remains the only eligible source type. Main playback decoding is unchanged. macOS/Linux require their own builds and playback checks; TV is separate.
