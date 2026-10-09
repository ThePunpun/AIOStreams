# AIOStreams Custom — seek previews, revision 9

Windows x64 portable test build. Extract into a fresh folder with the old app closed; keep all ZIP files together. You may copy the old `data` folder while both apps are closed to retain settings and earlier logs. Turn previews on in **Settings → Playback → Controls**. Use the launchers below; no PowerShell commands are needed. Run only one copy at a time.

**Core test: Launch 1, then Launch 2.** Launches 3 and 4 are an optional paired fast-seek comparison. Every launch produces a separate timestamped log. Collect just once at the end, including any black-screen relaunches. Logging out or changing Jellyfin servers does not erase logs.

## Launch 1 — Start-Default.cmd

Double-click **Start-Default.cmd**. This enables normal grid learning, Debrid background fill, strict health-gated Usenet background fill and HTTP hover-only previews.

### A. Debrid LOTR: cold hovers and five-minute fill

1. Play the same LOTR remux. As soon as you see playback, hover at **20:00** and hold for five seconds. Estimate the time until the first image appears.
2. Hover **40:00, 60:00, 80:00, 100:00 and 120:00**, five seconds at each. These are hovers, not clicks: the main video should keep playing. Record any delayed image, wrong colour or main playback buffering. Repeat those six positions to check cached speed.
3. Move the pointer away from the timeline and play untouched for **five minutes**. Then slowly sweep the entire timeline once. Note loading gaps and their approximate times. Check near playback and **five/fifteen minutes ahead and behind** where those positions exist.
4. Close the video and reopen the same release. Repeat the six hovers. The new session may use learned spacing; the previous session's spacing never changes during a sweep. Logs capture the interval automatically.
5. Move away and play for thirty seconds, then **pause for three minutes** with the pointer away from the timeline. Sweep once while paused. Note loading gaps and confirm the main picture/time remained paused. Resume and watch for one minute. Paused fill is allowed only with healthy buffer; a pause does not guarantee that the provider leaves enough buffer available.

On the first run with no history, this LOTR file should be around **51 seconds per point**, not eight seconds or 2.5 minutes. A later session may be around 44–45 seconds if the measured class median supports it. Reusing a thumbnail over part of the timeline is expected; the timestamp still follows the pointer. Five minutes is a measurement window, not a guaranteed full-cache deadline.

### B. Debrid episodes and tooltip

6. On the **same SDR episode**, hover three new distant positions for five seconds each. Then play **three minutes hands-off**, sweep once and repeat those positions. Record loading gaps and buffering.
7. Do the same on the **same 4K HDR episode**, using **five minutes hands-off**. Note natural colours and cold-hover delay.
8. On a file with chapters, cross several chapter boundaries and check the chapter name or numbered fallback beside the time. A long name should end in an ellipsis. On a file without chapters, only the timestamp should appear. Sweep quickly, hold one image for twenty seconds, then leave/re-enter: the box must stay at the same height and hide instantly on exit. Loading and unavailable states must also keep that height.
9. If a main-video seek buffers naturally, immediately hover another distant point during that wait. Record whether its preview appears and whether it recovers afterwards. Do not infer that preview work caused buffering which followed your click.

### C. Network recovery

10. On Debrid, turn previews off and on to start an empty cache. After playback has shown a frame, hover a distant point and **disconnect the PC's network for about twenty seconds**. Keep the pointer there. Reconnect and hold for up to **forty-five seconds**. Record whether the image returns without restarting. If it was already displayed before disconnecting, move to another distant point while offline. Main playback may buffer during the deliberate outage; label that as the recovery test.

### D. Usenet SDR, real DV P5 and optional P8

11. In this same app launch, log out of the Debrid Jellyfin server and into the Usenet server. No collection or restart is needed between servers.
12. Play a **Usenet SDR release five minutes hands-off before hovering**. Then check near playback and at roughly **10%, 30%, 50%, 70% and 90%** of its duration, holding each five seconds. Repeat them. Record first-hover delay, later cold/cached speed, loading gaps and any main buffering.
13. Repeat step 12 on the **real Usenet DV P5 release**. Report natural colours, green/purple output, persistent loading or “Preview unavailable.” The log will identify whether the profile was decoder-confirmed; appearance alone cannot establish P5.
14. **If you find P8**, repeat step 12. State whether it came from Usenet, Debrid or HTTP, and note the release/file name in your observations. For HTTP use step 15's demand-only expectations instead. Real P8 colours remain unverified until this test.

Usenet background is now enabled, but it starts only after a full observed minute with at least thirty seconds buffered, an idle main reader and no main buffering in the previous five minutes. It stops when health deteriorates. It may legitimately do **zero background captures** on a particular provider. This build has one preview decoder; the streaming backend determines actual NNTP connection use.

### E. HTTP addon

15. Switch to the HTTP addon and choose a **finite, seekable video**. Play **two minutes hands-off** before the first hover. Then hover near playback and roughly **10%, 30%, 50%, 70% and 90%**, five seconds each; repeat them. Note delay, colours and buffering. HTTP opens only on an accepted hover and does **no background fill**. A fully seekable source should show previews; a server that cannot seek should show “Preview unavailable” in the same stable frame.
16. If you already have an HTTP source which refuses seeking, check that case too. No need to add live or P2P sources. No parallel test is needed.

Close the app normally.

## Launch 2 — Start-Usenet-Demand-Only.cmd

Double-click **Start-Usenet-Demand-Only.cmd** and use the same Usenet server and releases as Launch 1. This launcher disables Usenet background only; it retains normal learning and demand previews.

1. Play the same **Usenet SDR release two minutes hands-off**, then hover the same six positions from step 12 for five seconds each; repeat them.
2. Repeat on the **same P5 release**, and P8 if available. Record colours, delays and any buffering.
3. If Launch 1 had buffering during untouched Usenet playback, extend this run's hands-off section to the same **five minutes** for a fairer comparison. Background captures should be zero in this launch.

Close normally. Comparing the two launch logs tells us whether strict Usenet background provides useful coverage without harming playback.

## Optional Launches 3 and 4 — paired fast-seek comparison

Do these only if you have time to measure the experiment. It is **not the normal default**. Both dedicated launchers disable learning so the grid history stays unchanged between the two runs. Do not run a normal-learning launcher between them. Use the same files and test order; network conditions can still vary.

### Launch 3: Start-Comparison-Default.cmd

1. Switch back to the Debrid server. Open the **same LOTR release** and immediately visit these twenty distant positions, holding each for five seconds: **10:00, 20:00, 30:00, 40:00, 50:00, 60:00, 70:00, 80:00, 90:00, 100:00, 110:00, 120:00, 130:00, 140:00, 150:00, 160:00, 170:00, 180:00, 190:00, 200:00**. Skip any point beyond the file's end. Then move away and play three minutes hands-off; repeat 20:00, 100:00 and 180:00.
2. Open the **same 4K HDR episode**. Immediately hover twenty points: **1:00, 2:30, 4:00, 5:30, 7:00, 8:30, 10:00, 11:30, 13:00, 14:30, 16:00, 17:30, 19:00, 20:30, 22:00, 23:30, 25:00, 26:30, 28:00, 29:30**, five seconds each. Skip beyond-end points. Then play three minutes hands-off and repeat 1:00, 14:30 and 28:00.
3. Note delays, colour and buffering. Close the app.

### Launch 4: Start-Fast-Seek.cmd

Repeat exactly Launch 3's files, positions and three-minute sections. Close afterwards. Logs distinguish truly cold from already cached positions, so do not reset or sweep before the twenty-position test.

Fast-seek changes only the private decoder: persistent HTTP requests, skipped loop filtering and no MKV end-duration probe. A speed improvement must not come with wrong colours, failed images or main buffering. After this experiment, use **Start-Default.cmd** for ordinary playback.

## Collect once — after all chosen launches

1. Close the app. If a startup was black, close/relaunch and note the time; those launches have their own logs too. This revision does not claim to fix all startup black screens.
2. In the extracted V9 folder, double-click **Collect-Preview-Logs.cmd**.
3. Wait for **Created Preview-Logs.zip with … logs**. Attach **Preview-Logs.zip**, optionally renamed **v9-preview-logs.zip**, and your observations.

**You do not collect between videos, servers or launches.** The collector includes every timestamped preview log still present in `data`; it replaces only the output ZIP. Up to forty launch logs are retained. Copying old `data` may include earlier revisions, which are identified by their headers. A force-close can omit a final summary but does not reset earlier files. Ordinary app logs are excluded because they can contain authenticated stream URLs.

For the other reviewer/chat, also attach **V9-CHANGES.md** and **v8-to-v9.patch**. The notes list exact changes and disagreements with the feedback. Report file/source, approximate cold delay, colour, loading gaps, buffering and whether the box stayed stable. No manual log editing is needed.
