# AIOStreams Custom

Windows x64 prototype for seek previews on cached debrid streams. This is a separate desktop client; connect it to your existing AIOStreams Jellyfin server. Your server can keep running `punpun-custom`.

Extract the entire ZIP into a new writable folder, then open **AIOStreams-Custom.exe**. Keep the `.portable` file, `libmpv-2.dll`, `vulkan` and `web` folders beside it. WebView2 is required, as in the official app. Sign-ins, settings and logs live in this folder's `data` directory. Updates are disabled in this prototype.

The timeline shows a preview above the hover time. **Settings → Playback → Controls → Seek previews** is on by default and remembered on this device. Previews appear only for streams the server identifies as cached debrid. Usenet, P2P, live streams, external players and sources without the necessary metadata are excluded.

## Test

1. Play a cached debrid stream. Let playback settle for a few seconds.
2. Hover near the playhead, then at several distant positions. Compare the first request with a repeat visit.
3. Move quickly across the timeline. Check that the shown image belongs to the indicated **Preview at** time. Nearby cached images may be approximate; the hover time remains the intended seek position.
4. Toggle previews off/on and compare startup, buffering and CPU use. Switching a release or episode should clear the old images.
5. Try another container/codec and a 4K/HDR stream. This is a prototype; compatibility and cold remote-seek latency need measurement.

Share the relevant **seek-previews-YYYY-MM-DD.log** from `data/logs`, plus which codec/container and whether the delay happens on first or repeated hover. This dedicated log records source-session IDs, cache/display timing, decoder open and seek/capture times, failures, and demuxer bytes when available. It contains no stream URLs, filenames or authentication headers. The ordinary desktop logs can contain URLs; use the dedicated preview log for this test.

Background work begins after playback is healthy, prepares up to 16 sparse samples, backs off when extraction is slow, and suspends when the main player buffers/seeks. Cached images display immediately. Unseen positions still require a remote seek. Demuxer bytes are an approximate decoder-read counter, not an exact network traffic measurement.

Windows x64 is the first test target. Most preview logic is shared, but macOS/Linux still require their own builds and playback checks. LG/Samsung TV requires a separate implementation.
