# AIOStreams Custom

Windows x64 prototype for seek previews on cached debrid streams. This is a separate desktop client; connect it to your existing AIOStreams Jellyfin server. Your server can keep running `punpun-custom`.

Extract the entire ZIP into a new writable folder, then open **AIOStreams-Custom.exe**. Keep the `.portable` file, `libmpv-2.dll`, `vulkan` and `web` folders beside it. WebView2 is required, as in the official app. Sign-ins, settings and logs live in this folder's `data` directory. Updates are disabled in this prototype.

The timeline shows a preview above the hover time. **Settings → Playback → Controls → Seek previews** is on by default and remembered on this device. Previews appear only for streams the server identifies as cached debrid. Usenet, P2P, live streams, external players and sources without the necessary metadata are excluded.

## Revision 2 test

1. Play a cached debrid stream. Let playback settle for a few seconds.
2. Hover near the playhead, then at several distant positions. Compare the first request with a repeat visit.
3. Seek the main player, then immediately hover an uncached position. Check that a brief skip no longer causes several seconds of preview downtime. Move quickly across the timeline. Check that the shown image belongs to the indicated **Preview at** time. Nearby cached images may be approximate; the hover time remains the intended seek position.
4. Toggle previews off/on and compare startup, buffering and CPU use. Switching a release or episode should clear the old images.
5. Try another container/codec and a 4K/HDR stream. This is a prototype; compatibility and cold remote-seek latency need measurement.

Share the relevant **seek-previews-YYYY-MM-DD.log** from `data/logs`, plus which codec/container and whether the delay happens on first or repeated hover. This dedicated log records source-session IDs, cache/display timing, decoder open and seek/capture times, failures, output dimensions, distance from the playhead, buffered-range membership, reopen count and work-budget use. Screenshot timing includes mpv filtering and JPEG encoding; file-read and base64 times are separate. It contains no stream URLs, filenames or authentication headers. The ordinary desktop logs can contain URLs; use the dedicated preview log for this test.

Background work begins after one second of healthy playback, checks up to 24 nearby positions (forward first, then backward), backs off when extraction is slow, and suspends when the main player buffers/seeks. Hover work can continue during a main-player seek, but waits during buffering. The paused preview decoder stays open through short interruptions and closes after 90 seconds without decoder work. Cached images display immediately; remote work waits 150 ms for the pointer to settle.

The native worker selects 5-second spacing below 30 minutes, otherwise 10 seconds. Cache matching uses the actual sampled time. A smaller, borderless thumbnail retains the last image while a replacement loads, and **Preview at** identifies a noticeable time difference. The session permits at most 192 capture attempts and 180 seconds of decoder work, with 96 images / 8 MiB of encoded images cached. After the work budget is used, existing cached previews remain available. Network bytes are unavailable; these limits are work limits, not a claimed traffic cap. HDR images are scaled before tonemapping.

Windows x64 is the first test target. Most preview logic is shared, but macOS/Linux still require their own builds and playback checks. LG/Samsung TV requires a separate implementation.

Close the custom app before replacing it. A fresh folder gives a clean test profile. To keep sign-ins/settings, copy only the old `data` folder into the new custom folder. Keep the previous test log separately if you delete the old folder.
