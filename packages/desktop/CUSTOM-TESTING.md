# AIOStreams Custom — seek previews, revision 6

Windows x64 portable build; cached debrid only. Close the old custom app and extract this ZIP into a fresh folder. Keep its contents together. You can copy the old `data` folder while both apps are closed to preserve login/settings.

## One launch, default settings

Double-click **Start-Default.cmd** beside **AIOStreams-Custom.exe**. This opens the app with the normal scale-first settings and clears the experimental variant for that launch. No PowerShell or three-variant comparison is needed. Enable previews in **Settings → Playback → Controls**.

Test all files in this one launch, switching video inside the app:

1. **Small SDR episode:** play for five minutes with the pointer away from the timeline. Then slowly sweep the entire timeline. Note any loading gaps and their approximate positions. Repeat those positions. Leave the enlarged timeline hit area and confirm the preview hides immediately. The first sweep is the coverage check; repeated hovers can fill missing slots, so keep them separate.
2. **Same LOTR DV7 remux:** hover at an unvisited point within five seconds of playback starting. Hold for ten seconds and note the first-image delay. Move away and let playback run untouched for five minutes. Then hover near playback, one minute ahead/behind, five minutes ahead/behind, and approximately 20:00, 60:00 and 100:00. Hold each for five seconds, then repeat. Report colours, loading delay and any main-player buffering.
3. **Same DV P5 episode:** hover three new positions for five seconds each and check natural colours, especially skin tones. Green/purple output is a failure. If the conversion is unavailable, that remains a supported fallback and does not change main playback. Add a DV P8 file if available.
4. Seek the main player, switch episode/release, and verify old previews disappear. Check ordinary dragging and the thumbnail outline/black bars.

Sampling adjusts to video length: **5 seconds for short episodes, roughly 25 seconds for the tested long LOTR remux, and up to 30 seconds for very long files**. The interval is the nearest 5-second multiple of duration / 600, bounded to 5–30 seconds. Hover and background use exactly the same interval. This restores the original duration-based density; it does not force ten-second sampling on movies. Actual screenshot timestamps can differ because the decoder seeks to keyframes; the UI shows the sampled time when it differs substantially from the hover time. A slot can also be covered by a neighbouring keyframe image.

Performance targets for a healthy source: at least 95% of a 23-minute SDR file's slots covered after five minutes untouched; cold SDR hovers around/below 0.5 seconds; LOTR's first image around/below five seconds after the first hover while playing. These are test targets, not guarantees. Startup cannot display uncached images immediately, and provider/network stalls can exceed them.

## Collect once, after all tests

Close the app completely. In the same extracted folder, double-click **Collect-Preview-Logs.cmd**. Wait for **Created Preview-Logs.zip with … logs**, then press any key to close the collector. Attach **Preview-Logs.zip** from that folder. You may rename the ZIP to **v6-preview-logs.zip**; keep the `.zip` extension.

Each app launch creates a separate timestamped `seek-previews-*.log` in `data/logs`. All videos within this one launch share its log. The collector includes every dedicated preview log present, including earlier copied logs. It refreshes the ZIP each time it runs; an earlier uploaded copy cannot acquire later entries. Collecting between videos or launches is unnecessary.

If the collector fails, select all `seek-previews-*.log` files in `data/logs` and compress them into a ZIP. Ordinary application logs are excluded by the collector because they can contain stream URLs.

Optional whole-app traffic comparison: use Windows Resource Monitor's Network tab over equal five-minute SDR hands-off runs with previews on/off. Its live rates are not a cumulative transferred-byte total. Record those rates or another actual traffic counter; preview demux queue sizes are not network bytes.

## What changed

- Hover and background sampling both use the same duration-based grid (5–30 seconds). The enlarged hit area remains; the exit timer is removed.
- Background scheduling chooses the closest missing slot, two ahead for each one behind, recomputing coverage as the playhead moves. It checks the whole slot, including both edges, rather than only its centre. Eviction cannot leave a permanently visited hole.
- The cumulative estimated-input cap is removed. Work pauses while hovering, seeking, paused, buffering, or below ten seconds buffered. At ten seconds or more it uses at most 25% decoder duty; with an idle/non-starved main reader and at least thirty seconds buffered, at most 50%. Buffering increases the cooldown. This limits activity, **not exact network traffic**.
- The former 90-second speculative limit is raised to two hours of actual background decoder work. Successful demand work has its own three-minute guard. There is a separate failed-work limit and bounded failure retries. These bounds prevent endless decoder activity without stopping a long movie after a few minutes.
- Both caches allow up to 8,641 slots within the unchanged 8 MiB encoded-size limit. Short episodes fit easily; very long/high-complexity files can reach the size limit before full coverage. Background stops at that limit instead of endlessly evicting and redoing its own work. Direct hovers can still replace older images.
- The decoder starts warming as soon as a main frame exists and about three seconds are buffered, or on the first hover after a main frame. Explicit hover captures can proceed during main buffering; background captures remain blocked. Decoder idle-close is ten minutes. A transient remote open has bounded retries.
- An opening screenshot cannot replace a more complete existing slot. Issued demand seeks still finish, except for the existing bounded early-preemption rule. Speculative work yields to hovers and worsening playback health.
- Logging adds cold `hover_to_image_ms`, first-hover wait reasons and coverage/near-playhead holes every thirty seconds plus session end. Cached images that are already available when entering a slot are not misclassified as cold merely because the pointer initially enters outside their coverage. First-image timing remains separate from seek/capture timing.
- Queue-size/rate diagnostics remain labelled as decoder state; exact transferred bytes remain unavailable. A decoder input underrun does not establish the cause of a network stall.
- Scale-first and in-memory JPEGs remain the default. The supplied V5 feedback reports natural colours on the tested P5 episode; Vulkan/libplacebo initialisation was recorded in the logs. This is a tester-confirmed regression reference, not automatic proof of colour accuracy for every P5 file/GPU. Unsupported conversion still shows preview unavailable.

Experimental environment variants remain available for developer diagnostics, but are no longer on the user test list. Main playback decoding, full-picture black bars and the preview toggle are preserved.
