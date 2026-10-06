# AIOStreams Custom

Windows x64 prototype for seek previews on cached debrid streams. This is a separate desktop client; connect it to your existing AIOStreams Jellyfin server. Your server can keep running `punpun-custom`.

Extract the entire ZIP into a new writable folder, then open **AIOStreams-Custom.exe**. Keep the `.portable` file, `libmpv-2.dll`, `vulkan` and `web` folders beside it. WebView2 is required, as in the official app. Sign-ins, settings and logs live in this folder's `data` directory. Updates are disabled in this prototype.

The timeline shows a preview above the hover time. **Settings → Playback → Controls → Seek previews** is on by default and remembered on this device. Previews appear only for streams the server identifies as cached debrid. Usenet, P2P, live streams, external players and sources without the necessary metadata are excluded.

## Revision 3 test

1. Play a cached debrid stream. Let playback settle for a few seconds.
2. Hover near the playhead, then at several distant positions. Compare the first request with a repeat visit.
3. Hold the pointer still over an uncached spot for at least 5 seconds. The controls and loading box should stay visible until you leave the timeline. Drag outside the timeline and back; the controls should stay visible throughout the drag. An uncached spot must show the loading indicator, never the previously hovered image.
4. Seek the main player, then immediately hover an uncached position. Check that a brief skip no longer causes several seconds of preview downtime. Move quickly across the timeline. Check that the shown image belongs to the indicated **Preview at** time. Nearby cached images may be approximate; the hover time remains the intended seek position.
5. Toggle previews off/on and compare startup, buffering and CPU use. Switching a release or episode should clear the old images.
6. Try another container/codec and a 4K/HDR stream. This is a prototype; compatibility and cold remote-seek latency need measurement.

Share the relevant **seek-previews-YYYY-MM-DD.log** from `data/logs`, plus which codec/container and whether the delay happens on first or repeated hover. This dedicated log records source-session IDs, cache/display timing, decoder open and seek/capture times, failures, output dimensions, distance from the playhead, buffered-range membership, reopen count and work-budget use. Screenshot timing includes mpv filtering and JPEG encoding; file-read and base64 times are separate. It contains no stream URLs, filenames or authentication headers. The ordinary desktop logs can contain URLs; use the dedicated preview log for this test.

Background work begins after one second of healthy playback, checks up to 24 nearby positions (forward first, then backward), backs off when extraction is slow, and suspends when the main player buffers/seeks. Hover work can continue during a main-player seek, but waits during buffering. The paused preview decoder stays open through short interruptions and closes after 90 seconds without decoder work. Cached images display immediately; remote work waits 150 ms for the pointer to settle.

The native worker selects 5-second spacing below 30 minutes, otherwise 10 seconds. Each image records its actual sampled time and the time requested from the decoder. Cache matching uses that approximate interval plus half a step at each edge. **Preview at** identifies a noticeable time difference. The dark, borderless loading box shows a spinner immediately on a miss; old images are not held while waiting.

Once a seek has been sent it finishes and caches its image even when the pointer moves or leaves. The newest settled hover is served next. After opening, the worker rechecks the current hover and playback health before seeking. A failed demand capture retries with a short delay, within the three-consecutive-failure limit, instead of leaving a stationary hover waiting forever. Session stop/replacement still cancels work, and frame waits have an 8-second timeout. Only completed captures count toward the 192-capture / 180-second work budget; open time and failed attempts are logged separately. Three consecutive decoder failures stop further extraction, protecting playback from repeated timeouts. Existing cached previews remain usable. Image caches hold at most 96 images / 8 MiB of encoded images; decoder buffering is separate. Network bytes are unavailable, so these are work limits, not a traffic cap.

HDR uses the v1 tonemap-first filter, with mpv's default back-cache and MKV duration probing restored. The dedicated log includes successful captures after pointer movement, hover enter/leave, input/output geometry, and decoder seeking/idle/cache state on slow waits and failures. The HDR remux regression must be verified on the real stream; generated fixtures cannot establish its cause.

### Optional controlled decoder trials

Start with the normal ZIP launch (baseline). Once that works, one option at a time can be selected in PowerShell from this folder:

```powershell
$env:AIOSTREAMS_PREVIEW_TEST_VARIANT = 'scale-first'
.\AIOStreams-Custom.exe
```

Allowed values are `scale-first`, `small-back-cache` (512 KiB), and `mkv-no-duration`. Each changes only that option relative to the baseline, and the log records the selected variant. Close the app completely between trials. Reset with `Remove-Item Env:AIOSTREAMS_PREVIEW_TEST_VARIANT` before launching the baseline again. No trials are enabled by default; do not combine them.

Windows x64 is the first test target. Most preview logic is shared, but macOS/Linux still require their own builds and playback checks. LG/Samsung TV requires a separate implementation.

Close the custom app before replacing it. A fresh folder gives a clean test profile. To keep sign-ins/settings, copy only the old `data` folder into the new custom folder. Keep the previous test log separately if you delete the old folder.
