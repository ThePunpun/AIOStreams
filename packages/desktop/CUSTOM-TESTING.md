# AIOStreams Custom — preview test v4

Windows x64 portable prototype, cached debrid only. Connect to your existing AIOStreams Jellyfin server; `punpun-custom` can stay on its current branch. Keep the official app installed. This custom app has its own `data` folder and no automatic updates.

## Install and start

1. Close the old custom app completely. Save its `data/logs/seek-previews-YYYY-MM-DD.log` before deleting its folder.
2. Extract the **entire ZIP** into a new writable folder, such as Downloads/AIOStreams-Custom-v4. Open the inner **AIOStreams-Custom** folder, which contains **AIOStreams-Custom.exe**, `.portable`, `libmpv-2.dll`, `vulkan` and `web`. Keep those files together. WebView2 is required, as in the official app.
3. For a clean profile, sign in again. To keep your custom sign-in/settings instead, copy only the old custom `data` folder into this new inner folder before launching. The official app's AppData folders are unaffected. Deleting the custom folder also deletes its local data/logs.
4. Open **AIOStreams-Custom.exe**. Check **Settings → Playback → Controls → Seek previews** is on (the default).

## Normal test — do this first

1. Start the same cached debrid LOTR remux. Record the playback position where you begin. Hover once within the first few seconds, then leave the timeline and let it play for 30 seconds. This measures startup separately from later requests.
2. Hover at six distant, previously unvisited positions, spaced at least one minute apart. Hold each for five seconds. Do not click or seek yet. Note roughly how long the image takes. Then repeat those same positions: cached previews should appear immediately.
3. Hold still over an uncached spot for 10 seconds. The controls and preview box should remain visible. Briefly move off the bar and back within about 0.2 seconds; the box should not blink away. Leave for longer: it should disappear, then the controls should resume their normal hide timer.
4. Sweep quickly across the timeline, then stop. The last image from a different uncached spot must not remain displayed while the new preview loads. Check both ends of the bar and the white thumb knob. Drag outside and back, then release; seeking and control visibility should still work.
5. Seek the main player, then hover a new position immediately. Check previews recover without reopening the decoder after every skip. Switch episode/release and check the old images disappear.
6. Play a non-HDR episode for **three minutes with the pointer away from the timeline**. Then hover around the playhead and a few minutes ahead/behind. Repeat on LOTR for **five minutes**, if practical. Note cache speed and any extra buffering. Prefetching deliberately waits for at least 15 seconds of main-player buffer; a slow/high-bitrate source may get very little background coverage.
7. Try a **4:3** video and a wide video. The outlined preview keeps a consistent 16:9 frame and contains the full picture, with black bars where needed. Black bars encoded into the picture itself also remain. There is no cropping, zooming or stretching. The time stays below the frame. Check a release without chapters: no invented chapter label should appear.
8. Turn previews off in playback settings and compare a few minutes of the same source. Turn them back on. Check playback, CPU use and buffering, and verify the control is remembered after restart.
9. If available, try HDR10/PQ, HLG and Dolby Vision files. Note the exact DV profile if you know it and whether colours look natural. Do not infer a profile from the title alone. Dolby Vision preview colour compatibility is not fully verified by generated fixtures.
10. Close the custom app to flush the session summary, then share the dedicated **seek-previews-YYYY-MM-DD.log** from this custom folder's **data/logs**. Include which tests/files you used, approximate test times and anything visually wrong. Keep ordinary app logs private; those may contain stream URLs.

## Optional HDR trials — same file, one option at a time

First complete the normal LOTR test. Trial order: **scale-first**, then **zscale-first**. Other variants are optional later. Compare each against a new normal/baseline session on the same release, same starting playback position and same six distant hover positions. Each session has a fresh preview cache. First-hover startup is separate from warmed decoder latency. A slow-wait log line alone does not prove a variant is at fault; compare paired timings and image quality.

### Open PowerShell in the correct folder

Close the custom app. In File Explorer, open the inner folder containing **AIOStreams-Custom.exe**. Click File Explorer's address bar, type **powershell**, and press Enter. This opens Windows PowerShell in that folder; administrator rights are unnecessary. All commands below go in that PowerShell window, one line at a time.

To ensure a baseline:

```powershell
Remove-Item Env:AIOSTREAMS_PREVIEW_TEST_VARIANT -ErrorAction SilentlyContinue
.\AIOStreams-Custom.exe
```

Close the custom app fully before the next trial. If unsure, open Task Manager and confirm **AIOStreams-Custom.exe** is no longer running. Then, in the **same PowerShell window**:

```powershell
$env:AIOSTREAMS_PREVIEW_TEST_VARIANT = 'scale-first'
.\AIOStreams-Custom.exe
```

Use the same LOTR release, position and six uncached spots. Hold each five seconds, check colour/shape and note delays. Close the app fully, then run the next separate trial:

```powershell
$env:AIOSTREAMS_PREVIEW_TEST_VARIANT = 'zscale-first'
.\AIOStreams-Custom.exe
```

Close the app fully afterwards and return to the normal path:

```powershell
Remove-Item Env:AIOSTREAMS_PREVIEW_TEST_VARIANT -ErrorAction SilentlyContinue
.\AIOStreams-Custom.exe
```

`Remove-Item Env:...` removes **only this temporary setting from this PowerShell session**. It deletes no app, profile, log or Windows system setting. Closing that PowerShell window also removes its process-local setting; an app already launched keeps its inherited setting until it exits. Launching from Explorer does not inherit a variable you set only in this console. The log records `test_variant` for each decoder open.

| Variant | Single decoder change from the normal path |
| --- | --- |
| `scale-first` | Resize before the HDR float conversion/tone mapping |
| `zscale-first` | Resize within the first zscale conversion before tone mapping |
| `skip-loop-filter` | Skip the decoder's loop filter for preview images |
| `threads-4` | Four software decoder threads instead of two |
| `persistent-http` | Request FFmpeg HTTP connection reuse; effect depends on the server |
| `small-back-cache` | 512 KiB backward demux cache |
| `mkv-no-duration` | Disable MKV video-duration probing |

Do not combine variants. Hardware decoding remains off for the preview worker; a hardware trial needs separate device-specific checks. The main player's decoder is unchanged.

## What changed and what the log measures

- A thin neutral outline, rounded frame, darker loading background and time below the preview. A consistent 16:9 frame contains the full picture and preserves black bars without cropping, zooming or stretching. No image from a different hovered location is held while waiting.
- Cached images remain instant; remote requests settle for **75 ms**, down from 150 ms. Momentary timeline exits get **200 ms** grace. Hover telemetry describes settled requests; a cached image can already be visible before its settled display event is logged.
- An already-decoded opening frame is captured without another seek. Its one-time file operation is logged as `open_frame=true` rather than hidden inside the first hover seek.
- HDR startup waits for the filter's asynchronous refresh to finish before caching that opening frame. A slow 4K fixture exposed an unconverted opening screenshot; the colour test now compares against a reference captured after a completed refresh. `hdr_filter_ready_ms` measures this preparation separately; `opened_ms` includes it.
- Background seeks yield as soon as you hover. Issued **demand** seeks finish and cache their images so long-GOP files cannot be starved by repeated pointer cancellation. The newest settled hover runs next.
- Background extraction uses up to **90 seconds of decoder work**, with four forward positions for each backward position, rather than 24 positions. Main playback must be calm with at least **15 seconds buffered ahead**. Unknown buffer depth disables background work. Buffering doubles the back-off. A fast source can also prefetch one directional neighbor after the pointer rests; it requires at least three measured seeks with median below 500 ms and healthy buffering.
- Duration determines spacing in 5-second multiples from 5 to 30 seconds, aiming for about 600 positions. A 23-minute episode uses 5 seconds, a two-hour film 10 seconds, and a roughly four-hour film 25 seconds.
- Native and UI caches each hold up to **400 images / 8 MiB encoded**. Completed captures have a 180-second work budget and a 1,200-capture safety cap; actual failed work has a separate 90-second ceiling. Speculative work, including interruptions, counts toward its 90-second background budget. Expected interruptions do not spend the failure budget or disable demand work. Three consecutive decoder errors stop extraction; speculative cancellation does not count as an error. Cached images remain usable.
- The log separates seek, screenshot (including filtering/JPEG encoding), file open/read/close/delete and base64 time. It flags image I/O operations taking at least 250 ms, but does not diagnose antivirus activity. The earlier combined `read_ms` also included opening and deleting the file.
- Each capture logs geometry, actual sampled time/coverage (including keyframes landing after the request), playhead distance, main buffered-range membership, demux queue bytes and raw input rate. **Demux queue bytes are current buffered packets, not cumulative downloaded traffic.** No transferred-byte cap is claimed.
- SDR uses scaling; PQ/HDR10 and HLG are converted to SDR thumbnails. HDR10+ uses its PQ base rather than full dynamic-metadata rendering. Known compatible Dolby Vision profiles use the restored base layer; unsupported or unknown DV requiring reshaping stops previews with a logged reason. This is not full Dolby Vision rendering, and real DV colour checks are still needed.

Optional traffic comparison: open Windows Resource Monitor → Network, select the custom app, and compare five minutes of the same playback with previews on/off. This measures the whole app (player plus previews), varies with buffering, and is not an isolated preview-worker traffic measurement.

Most logic is shared, but macOS/Linux require their own builds and playback checks; TV requires a separate implementation. Usenet and P2P remain excluded in this test.
