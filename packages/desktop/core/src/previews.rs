//! Session-scoped seek images. A second, small decoder never seeks the playing decoder.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::bridge::Outbound;
use crate::mpv::{Event, Kind, Mpv};
use crate::player::Emit;

const MAX_CAPTURES: usize = 192;
const MAX_WORK_MS: u64 = 180_000;
const MAX_IMAGES: usize = 96;
const MAX_BACKGROUND: usize = 24;

#[derive(Clone)]
struct Session {
    id: String,
    url: String,
    wanted: Option<f64>,
}

#[derive(Default)]
struct State {
    generation: u64,
    session: Option<Session>,
    shutdown: bool,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct Previews {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl Previews {
    pub fn new(library: &Path, main: Arc<Mpv>, emit: Emit) -> Result<Self, String> {
        let shared: Shared = Arc::default();
        let thread = std::thread::Builder::new()
            .name("seek-previews".into())
            .spawn({
                let shared = shared.clone();
                let library = library.to_path_buf();
                move || run(&library, &main, &emit, &shared)
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn start(&self, id: String, url: String) {
        let mut state = self.shared.0.lock().unwrap();
        state.generation += 1;
        state.session = Some(Session {
            id,
            url,
            wanted: None,
        });
        self.shared.1.notify_one();
    }

    pub fn stop(&self, id: &str) {
        let mut state = self.shared.0.lock().unwrap();
        if state.session.as_ref().is_some_and(|s| s.id == id) {
            state.generation += 1;
            state.session = None;
            self.shared.1.notify_one();
        }
    }

    pub fn request(&self, id: &str, seconds: Option<f64>) {
        let mut state = self.shared.0.lock().unwrap();
        if let Some(s) = state.session.as_mut().filter(|s| s.id == id) {
            s.wanted = seconds.filter(|v| valid_position(*v));
            self.shared.1.notify_one();
        }
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        {
            let mut state = self.shared.0.lock().unwrap();
            state.shutdown = true;
            state.generation += 1;
            self.shared.1.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn valid_position(seconds: f64) -> bool {
    seconds.is_finite() && (0.0..=24.0 * 3600.0).contains(&seconds)
}

fn step_for(duration: f64) -> f64 {
    if duration > 0.0 && duration < 1800.0 {
        5.0
    } else {
        10.0
    }
}

fn bucket(seconds: f64, step: f64) -> Option<u32> {
    valid_position(seconds).then(|| (seconds / step).floor() as u32)
}

fn source_active(main: &Mpv, url: &str) -> bool {
    main.get_property("path", Kind::String)
        .and_then(|v| v.as_str().map(str::to_owned))
        .as_deref()
        == Some(url)
        && main.get_property("idle-active", Kind::Flag) == Some(false.into())
        && number(main, "time-pos").is_some()
}

fn hover_allowed(main: &Mpv, url: &str) -> bool {
    source_active(main, url)
        && main.get_property("paused-for-cache", Kind::Flag) != Some(true.into())
}

fn number(mpv: &Mpv, name: &str) -> Option<f64> {
    mpv.get_property(name, Kind::Double)?
        .as_f64()
        .filter(|v| v.is_finite())
}

fn active(shared: &Shared, generation: u64) -> bool {
    let state = shared.0.lock().unwrap();
    !state.shutdown && state.generation == generation && state.session.is_some()
}

fn healthy(main: &Mpv, url: &str) -> bool {
    hover_allowed(main, url)
        && main.get_property("seeking", Kind::Flag) != Some(true.into())
        && number(main, "demuxer-cache-duration").is_none_or(|v| v == 0.0 || v >= 1.0)
}

// Once a seek is issued, pointer movement and main-player seeks cannot discard it.
// New work is gated in run(); only stopping/replacing the session cancels in-flight work.
fn wait_frame(
    mpv: &Mpv,
    shared: &Shared,
    generation: u64,
    timeout: Duration,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    let slow_at = Instant::now() + Duration::from_secs(2);
    let mut logged_slow = false;
    while Instant::now() < deadline {
        if !active(shared, generation) {
            return Err("cancelled");
        }
        if !logged_slow && Instant::now() >= slow_at {
            logged_slow = true;
            let session = shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .map(|s| s.id.clone());
            if let Some(session) = session {
                log_decoder_state(mpv, &session, "waiting-frame-over-2s");
            }
        }
        match mpv.wait_event(0.05, |_| false) {
            Some(Event::PlaybackRestart) => return Ok(()),
            Some(
                Event::EndFile {
                    reason: "error", ..
                }
                | Event::Shutdown,
            ) => {
                return Err("decode-error");
            }
            _ => {}
        }
    }
    Err("timeout")
}

#[derive(Clone)]
struct Image {
    bucket: u32,
    position: f64,
    covers_until: f64,
    data: String,
}

#[derive(Default)]
struct Stats {
    generated: usize,
    background: usize,
    hits: usize,
    errors: usize,
    completed_after_move: usize,
    attempts: usize,
    opens: usize,
    work_ms: u64,
    failed_work_ms: u64,
    waiting: usize,
}

struct Decoder {
    mpv: Mpv,
    dir: PathBuf,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.mpv.command(&["stop"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn open(
    library: &Path,
    main: &Mpv,
    session: &Session,
    shared: &Shared,
    generation: u64,
    reopened: bool,
) -> Result<Decoder, &'static str> {
    // Developer A/B trials are opt-in and mutually exclusive. The default restores
    // the decoder settings that produced thumbnails on the user's HDR remux in v1.
    let variant = match std::env::var("AIOSTREAMS_PREVIEW_TEST_VARIANT").as_deref() {
        Ok("scale-first") => "scale-first",
        Ok("small-back-cache") => "small-back-cache",
        Ok("mkv-no-duration") => "mkv-no-duration",
        _ => "baseline",
    };
    let mut options = vec![
        ("config", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("vo", "null"),
        ("audio", "no"),
        ("sub", "no"),
        ("pause", "yes"),
        ("idle", "yes"),
        ("keep-open", "yes"),
        ("terminal", "no"),
        ("cache", "yes"),
        ("demuxer-readahead-secs", "0"),
        ("demuxer-max-bytes", "512KiB"),
        ("network-timeout", "5"),
        ("vd-lavc-threads", "2"),
        ("hwdec", "no"),
        ("vf", "scale=320:-2"),
        ("screenshot-format", "jpg"),
        ("screenshot-jpeg-quality", "70"),
        ("hr-seek", "no"),
    ];
    match variant {
        "small-back-cache" => options.push(("demuxer-max-back-bytes", "512KiB")),
        "mkv-no-duration" => options.push(("demuxer-mkv-probe-video-duration", "no")),
        _ => {}
    }
    let mpv = Mpv::new(library, &options).map_err(|_| "decoder-init")?;
    for prop in [
        "http-header-fields",
        "user-agent",
        "referrer",
        "http-proxy",
        "tls-verify",
        "vid",
        "edition",
    ] {
        if let Some(value) = main
            .get_property(prop, Kind::String)
            .and_then(|v| v.as_str().map(str::to_owned))
        {
            // These are the playing decoder's options, never arbitrary page-provided options.
            let _ = mpv.set_property(prop, &value);
        }
    }
    let position = number(main, "time-pos").unwrap_or(0.0);
    let _ = mpv.set_property("start", &position.to_string());
    let dir = std::env::temp_dir().join(format!(
        "AIOStreams-Custom-previews-{}-{generation}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|_| "temp-directory")?;
    let decoder = Decoder { mpv, dir };
    let start = Instant::now();
    decoder
        .mpv
        .command(&["loadfile", &session.url])
        .map_err(|_| "open-command")?;
    wait_frame(&decoder.mpv, shared, generation, Duration::from_secs(12))?;
    if decoder.mpv.get_property("seekable", Kind::Flag) != Some(true.into()) {
        return Err("not-seekable");
    }
    let gamma = decoder
        .mpv
        .get_property("video-dec-params", Kind::Json)
        .and_then(|v| v.get("gamma").and_then(|v| v.as_str()).map(str::to_owned));
    if matches!(gamma.as_deref(), Some("pq" | "hlg")) {
        let filter = if variant == "scale-first" {
            "lavfi=[scale=320:-2:flags=area,zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,format=yuv420p]"
        } else {
            "lavfi=[zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,scale=320:-2,format=yuv420p]"
        };
        decoder
            .mpv
            .set_property("vf", filter)
            .map_err(|_| "hdr-filter")?;
    }
    log::info!(target: "seek_preview", "session={} opened_ms={} reopened={reopened} hdr={} filter={} threads=2 test_variant={variant}", session.id, start.elapsed().as_millis(), matches!(gamma.as_deref(), Some("pq" | "hlg")), if matches!(gamma.as_deref(), Some("pq" | "hlg")) { if variant == "scale-first" { "scale-first" } else { "tonemap-first-v1" } } else { "scale-only" });
    log_decoder_state(&decoder.mpv, &session.id, "opened");
    Ok(decoder)
}

struct Capture {
    target: u32,
    demand: bool,
    step: f64,
    reopened: bool,
}

fn capture(
    decoder: &Decoder,
    main: &Mpv,
    session: &Session,
    shared: &Shared,
    generation: u64,
    request: Capture,
) -> Result<Image, &'static str> {
    let Capture {
        target,
        demand,
        step,
        reopened,
    } = request;
    // Drain old restart events so an earlier seek cannot complete this request.
    while decoder.mpv.wait_event(0.0, |_| false).is_some() {}
    let wanted_s = ((f64::from(target) + 0.5) * step)
        .min((number(main, "duration").unwrap_or(86400.0) - 0.05).max(0.0));
    let playhead = number(main, "time-pos");
    let main_buffered = in_buffered_range(main, wanted_s);
    let started = Instant::now();
    decoder
        .mpv
        .command(&["seek", &wanted_s.to_string(), "absolute", "keyframes"])
        .map_err(|_| "seek-command")?;
    log::info!(target: "seek_preview", "session={} bucket={target} seek_issued=true demand={demand} target_s={wanted_s:.3}", session.id);
    wait_frame(&decoder.mpv, shared, generation, Duration::from_secs(8))?;
    if !active(shared, generation) {
        return Err("cancelled");
    }
    let seek_ms = started.elapsed().as_millis();
    let position = number(&decoder.mpv, "time-pos").ok_or("no-position")?;
    let path = decoder.dir.join("frame.jpg");
    let screenshot_at = Instant::now();
    decoder
        .mpv
        .command(&["screenshot-to-file", &path.to_string_lossy(), "video"])
        .map_err(|_| "screenshot")?;
    let screenshot_ms = screenshot_at.elapsed().as_millis();
    let read_at = Instant::now();
    let bytes = std::fs::read(&path).map_err(|_| "read-image")?;
    let _ = std::fs::remove_file(&path);
    if bytes.len() > 512 * 1024 || !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("invalid-image");
    }
    let read_ms = read_at.elapsed().as_micros() as f64 / 1000.0;
    let encode_at = Instant::now();
    let data = format!("data:image/jpeg;base64,{}", STANDARD.encode(&bytes));
    let base64_ms = encode_at.elapsed().as_micros() as f64 / 1000.0;
    let output = decoder.mpv.get_property("video-out-params", Kind::Json);
    let width = output
        .as_ref()
        .and_then(|v| v.get("w"))
        .and_then(|v| v.as_u64());
    let height = output
        .as_ref()
        .and_then(|v| v.get("h"))
        .and_then(|v| v.as_u64());
    log::info!(target: "seek_preview", "session={} bucket={} target_s={wanted_s:.3} sampled_s={position:.3} covers_until_s={wanted_s:.3} offset_s={:.3} demand={} seek_ms={seek_ms} capture_ms={} screenshot_ms={screenshot_ms} read_ms={read_ms:.3} base64_ms={base64_ms:.3} image_bytes={} playhead_distance_s={:?} main_buffered={main_buffered:?} reopened={reopened} vf_width={width:?} vf_height={height:?}", session.id, target, position - wanted_s, demand, screenshot_at.elapsed().as_millis(), bytes.len(), playhead.map(|v| wanted_s-v));
    Ok(Image {
        bucket: target,
        position,
        covers_until: wanted_s.max(position),
        data,
    })
}

fn log_decoder_state(mpv: &Mpv, session: &str, reason: &str) {
    let cache = mpv.get_property("demuxer-cache-state", Kind::Json);
    let mut safe_cache = serde_json::Map::new();
    for key in [
        "fw-bytes",
        "total-bytes",
        "reader-pts",
        "cache-end",
        "eof",
        "underrun",
        "idle",
        "raw-input-rate",
    ] {
        if let Some(value) = cache.as_ref().and_then(|s| s.get(key))
            && (value.is_number() || value.is_boolean())
        {
            safe_cache.insert(key.into(), value.clone());
        }
    }
    let output = mpv.get_property("video-out-params", Kind::Json);
    let input = mpv.get_property("video-dec-params", Kind::Json);
    log::info!(target: "seek_preview", "session={session} decoder_state={reason} seeking={:?} core_idle={:?} paused_for_cache={:?} time_pos={:?} has_video_out={} input_width={:?} input_height={:?} output_width={:?} output_height={:?} cache={}",
        mpv.get_property("seeking", Kind::Flag).and_then(|v| v.as_bool()),
        mpv.get_property("core-idle", Kind::Flag).and_then(|v| v.as_bool()),
        mpv.get_property("paused-for-cache", Kind::Flag).and_then(|v| v.as_bool()),
        number(mpv, "time-pos"), output.is_some(),
        input.as_ref().and_then(|v| v.get("w")).and_then(|v| v.as_u64()),
        input.as_ref().and_then(|v| v.get("h")).and_then(|v| v.as_u64()),
        output.as_ref().and_then(|v| v.get("w")).and_then(|v| v.as_u64()),
        output.as_ref().and_then(|v| v.get("h")).and_then(|v| v.as_u64()),
        serde_json::Value::Object(safe_cache));
}

fn in_buffered_range(main: &Mpv, seconds: f64) -> Option<bool> {
    let state = main.get_property("demuxer-cache-state", Kind::Json)?;
    let ranges = state.get("seekable-ranges")?.as_array()?;
    Some(ranges.iter().any(|r| {
        r.get("start")
            .and_then(|v| v.as_f64())
            .zip(r.get("end").and_then(|v| v.as_f64()))
            .is_some_and(|(start, end)| seconds >= start && seconds <= end)
    }))
}

fn nearby(cache: &VecDeque<Image>, seconds: f64, tolerance: f64) -> Option<&Image> {
    cache
        .iter()
        .filter(|i| seconds >= i.position - tolerance && seconds <= i.covers_until + tolerance)
        .min_by(|a, b| {
            (a.position - seconds)
                .abs()
                .total_cmp(&(b.position - seconds).abs())
        })
}

fn background_target(position: f64, duration: f64, step: f64, index: usize) -> Option<u32> {
    // Four ahead, then one behind. Stay close to the current playhead.
    let offset = if index == 0 {
        0.0
    } else if index.is_multiple_of(5) {
        -((index / 5) as f64)
    } else {
        (index - index / 5) as f64
    };
    bucket(
        (position + offset * step).clamp(0.0, (duration - 1.0).max(0.0)),
        step,
    )
}

fn finish_request(shared: &Shared, generation: u64, target: u32, step: f64) {
    let mut state = shared.0.lock().unwrap();
    if state.generation == generation
        && let Some(s) = state.session.as_mut()
        && s.wanted.and_then(|s| bucket(s, step)) == Some(target)
    {
        s.wanted = None;
    }
}

fn run(library: &Path, main: &Mpv, emit: &Emit, shared: &Shared) {
    loop {
        let (generation, session) = {
            let mut state = shared.0.lock().unwrap();
            while !state.shutdown && state.session.is_none() {
                state = shared.1.wait(state).unwrap();
            }
            if state.shutdown {
                return;
            }
            (state.generation, state.session.clone().unwrap())
        };
        let mut stats = Stats::default();
        let mut cache: VecDeque<Image> = VecDeque::new();
        let mut decoder = None;
        let mut healthy_since = None;
        let mut background_at = Instant::now();
        let mut retry_at = Instant::now();
        let mut last_work = Instant::now();
        let mut backoff = Duration::from_millis(500);
        let mut failed = false;
        let mut consecutive_errors = 0;
        let mut status = "";
        let mut step = 10.0;
        let mut configured = false;
        let mut fresh_open = false;
        log::info!(target: "seek_preview", "session={} start cached_debrid=true max_background={MAX_BACKGROUND} max_completed_captures={MAX_CAPTURES} max_work_ms={MAX_WORK_MS} idle_close_s=90", session.id);
        while active(shared, generation) {
            let calm = healthy(main, &session.url);
            let hover_ok = hover_allowed(main, &session.url);
            if !configured
                && source_active(main, &session.url)
                && let Some(duration) = number(main, "duration")
            {
                step = step_for(duration);
                configured = true;
                status = "";
                log::info!(target: "seek_preview", "session={} step_ms={} duration_s={duration:.3}", session.id, (step*1000.0) as u32);
            }
            let next_status = if failed {
                "unavailable"
            } else if !hover_ok {
                "waiting"
            } else {
                "ready"
            };
            if status != next_status {
                status = next_status;
                if status == "waiting" {
                    stats.waiting += 1;
                }
                emit(Outbound::PreviewStatus {
                    session: session.id.clone(),
                    state: status.into(),
                    step_ms: (step * 1000.0) as u32,
                });
                log::info!(target: "seek_preview", "session={} state={status} decoder_open={}", session.id, decoder.is_some());
            }
            if decoder.is_some() && last_work.elapsed() > Duration::from_secs(90) {
                decoder = None;
                log::info!(target: "seek_preview", "session={} decoder_closed=idle", session.id);
            }
            if calm {
                healthy_since.get_or_insert_with(Instant::now);
            } else {
                healthy_since = None;
            }
            if !hover_ok || failed || !configured {
                std::thread::park_timeout(Duration::from_millis(100));
                continue;
            }
            let wanted_seconds = shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .and_then(|s| s.wanted);
            let wanted = wanted_seconds.and_then(|s| bucket(s, step));
            let demand = wanted.is_some();
            let target = wanted.or_else(|| {
                if !calm || stats.background >= MAX_BACKGROUND || Instant::now() < background_at {
                    return None;
                }
                background_target(
                    number(main, "time-pos")?,
                    number(main, "duration")?,
                    step,
                    stats.background,
                )
            });
            let Some(target) = target else {
                let state = shared.0.lock().unwrap();
                let _ = shared
                    .1
                    .wait_timeout(state, Duration::from_millis(100))
                    .unwrap();
                continue;
            };
            if let Some(image) = nearby(
                &cache,
                wanted_seconds.unwrap_or((f64::from(target) + 0.5) * step),
                step / 2.0,
            ) {
                if demand {
                    stats.hits += 1;
                    deliver(emit, &session, image, true, 0);
                    finish_request(shared, generation, target, step);
                } else {
                    stats.background += 1;
                }
                continue;
            }
            if stats.generated >= MAX_CAPTURES || stats.work_ms >= MAX_WORK_MS {
                failed = true;
                decoder = None;
                log::info!(target: "seek_preview", "session={} budget_exhausted=completed_work generated={} work_ms={}", session.id, stats.generated, stats.work_ms);
                continue;
            }
            if decoder.is_none() {
                // Only the initial open needs a calm-start delay. Short seeks never discard the decoder.
                if stats.opens == 0
                    && healthy_since.is_none_or(|t| t.elapsed() < Duration::from_secs(1))
                {
                    std::thread::park_timeout(Duration::from_millis(100));
                    continue;
                }
                match open(library, main, &session, shared, generation, stats.opens > 0) {
                    Ok(d) => {
                        decoder = Some(d);
                        stats.opens += 1;
                        fresh_open = stats.opens > 1;
                        last_work = Instant::now();
                    }
                    Err("cancelled") => break,
                    Err(reason) => {
                        log::info!(target: "seek_preview", "session={} open_failed={reason}", session.id);
                        stats.errors += 1;
                        failed = true;
                    }
                }
                // Opening can take seconds. Re-read the newest hover and playback
                // health before issuing the first seek rather than using stale work.
                continue;
            }
            if Instant::now() < retry_at {
                std::thread::park_timeout(Duration::from_millis(100));
                continue;
            }
            let started = Instant::now();
            stats.attempts += 1;
            if !demand {
                stats.background += 1;
            }
            let result = capture(
                decoder.as_ref().unwrap(),
                main,
                &session,
                shared,
                generation,
                Capture {
                    target,
                    demand,
                    step,
                    reopened: fresh_open,
                },
            );
            if let Err(reason) = result.as_ref() {
                stats.failed_work_ms += started.elapsed().as_millis() as u64;
                log::info!(target: "seek_preview", "session={} bucket={target} demand={demand} aborted={reason} elapsed_ms={}", session.id, started.elapsed().as_millis());
                log_decoder_state(&decoder.as_ref().unwrap().mpv, &session.id, reason);
            }
            let completed = result.is_ok();
            match result {
                Ok(image) => {
                    if !active(shared, generation) {
                        break;
                    }
                    stats.generated += 1;
                    stats.work_ms += started.elapsed().as_millis() as u64;
                    consecutive_errors = 0;
                    let newest = shared
                        .0
                        .lock()
                        .unwrap()
                        .session
                        .as_ref()
                        .and_then(|s| s.wanted);
                    if demand && newest.and_then(|s| bucket(s, step)) != Some(target) {
                        stats.completed_after_move += 1;
                        log::info!(target: "seek_preview", "session={} bucket={target} completed_after_move=true next_bucket={:?}", session.id, newest.and_then(|s| bucket(s, step)));
                    }
                    deliver(
                        emit,
                        &session,
                        &image,
                        false,
                        started.elapsed().as_millis() as u64,
                    );
                    cache.push_back(image);
                    while cache.len() > MAX_IMAGES
                        || cache.iter().map(|i| i.data.len()).sum::<usize>() > 8 * 1024 * 1024
                    {
                        cache.pop_front();
                    }
                    backoff = Duration::from_millis(
                        (started.elapsed().as_millis() as u64 * 3).clamp(500, 5000),
                    );
                    fresh_open = false;
                }
                Err("cancelled") => break,
                Err(reason) => {
                    stats.errors += 1;
                    consecutive_errors += 1;
                    retry_at = Instant::now() + Duration::from_millis(500);
                    log::info!(target: "seek_preview", "session={} bucket={target} failed={reason}", session.id);
                    if consecutive_errors >= 3 {
                        failed = true;
                        decoder = None;
                    }
                }
            }
            last_work = Instant::now();
            if demand && completed {
                finish_request(shared, generation, target, step);
            }
            background_at = Instant::now() + backoff;
        }
        drop(decoder);
        log::info!(target: "seek_preview", "session={} summary generated={} background_candidates={} native_hits={} errors={} completed_after_move={} opens={} waiting_intervals={} attempts={} work_ms={} failed_work_ms={} network_bytes=unavailable", session.id, stats.generated, stats.background, stats.hits, stats.errors, stats.completed_after_move, stats.opens, stats.waiting, stats.attempts, stats.work_ms, stats.failed_work_ms);
    }
}

fn deliver(emit: &Emit, session: &Session, image: &Image, cached: bool, elapsed_ms: u64) {
    emit(Outbound::PreviewFrame {
        session: session.id.clone(),
        bucket: image.bucket,
        position: image.position,
        covers_until: image.covers_until,
        image: image.data.clone(),
        cached,
        elapsed_ms,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestLog;
    static OPEN_COUNT: AtomicUsize = AtomicUsize::new(0);
    // Change/clear the hover after the real seek command has been accepted, before waiting.
    static CHANGE_ON_SEEK: Mutex<Option<(Shared, Option<f64>)>> = Mutex::new(None);
    impl log::Log for TestLog {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.target() == "seek_preview"
        }
        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                let message = record.args().to_string();
                if message.contains("opened_ms=") {
                    OPEN_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                if message.contains("seek_issued=true demand=true")
                    && let Some((shared, wanted)) = CHANGE_ON_SEEK.lock().unwrap().take()
                    && let Some(session) = shared.0.lock().unwrap().session.as_mut()
                {
                    session.wanted = wanted;
                }
                println!("{message}");
            }
        }
        fn flush(&self) {}
    }
    static TEST_LOG: TestLog = TestLog;

    #[test]
    fn rejects_invalid_positions_and_groups_nearby_requests() {
        assert_eq!(bucket(f64::NAN, 5.0), None);
        assert_eq!(bucket(f64::INFINITY, 5.0), None);
        assert_eq!(bucket(-1.0, 5.0), None);
        assert_eq!(bucket(1e20, 5.0), None);
        assert_eq!(bucket(9.9, 5.0), Some(1));
        assert_eq!(bucket(10.0, 5.0), Some(2));
    }

    #[test]
    fn adaptive_grid_and_sample_time_cache() {
        assert_eq!(step_for(1799.0), 5.0);
        assert_eq!(step_for(1800.0), 10.0);
        assert_eq!(step_for(0.0), 10.0);
        assert_eq!(background_target(60.0, 3600.0, 10.0, 1), Some(7));
        assert_eq!(background_target(60.0, 3600.0, 10.0, 5), Some(5));
        assert_eq!(background_target(0.0, 3600.0, 10.0, 5), Some(0));
        let cache = VecDeque::from([Image {
            bucket: 2,
            position: 7.0,
            covers_until: 25.0,
            data: String::new(),
        }]);
        assert!(nearby(&cache, 29.9, 5.0).is_some());
        assert!(nearby(&cache, 30.1, 5.0).is_none());
        assert!(nearby(&cache, 1.9, 5.0).is_none());
    }

    #[test]
    #[ignore = "requires libmpv and the authenticated HTTP fixture"]
    fn remote_decoder_and_session_cleanup() {
        log::set_logger(&TEST_LOG).unwrap();
        log::set_max_level(log::LevelFilter::Info);
        let library = PathBuf::from(std::env::var("PREVIEW_TEST_LIBMPV").unwrap());
        let url = std::env::var("PREVIEW_TEST_URL").unwrap();
        let main = Arc::new(
            Mpv::new(
                &library,
                &[
                    ("config", "no"),
                    ("load-scripts", "no"),
                    ("vo", "null"),
                    ("audio", "no"),
                    ("pause", "yes"),
                    ("idle", "yes"),
                    ("cache", "yes"),
                ],
            )
            .unwrap(),
        );
        main.set_property(
            "http-header-fields",
            "Authorization: Bearer preview-fixture",
        )
        .unwrap();
        let shared: Shared = Arc::default();
        {
            let mut s = shared.0.lock().unwrap();
            s.generation = 1;
            s.session = Some(Session {
                id: "test".into(),
                url: url.clone(),
                wanted: None,
            });
        }
        main.command(&["loadfile", &url]).unwrap();
        wait_frame(&main, &shared, 1, Duration::from_secs(10)).unwrap();
        if let Ok(expected) = std::env::var("PREVIEW_TEST_EXPECT_GAMMA") {
            assert_eq!(
                main.get_property("video-dec-params", Kind::Json)
                    .and_then(|v| v.get("gamma").and_then(|v| v.as_str()).map(str::to_owned)),
                Some(expected),
                "fixture did not exercise the expected colour transfer"
            );
        }
        let (send, receive) = std::sync::mpsc::channel();
        let worker = Previews::new(
            &library,
            main.clone(),
            Arc::new(move |m| {
                send.send(m).unwrap();
            }),
        )
        .unwrap();
        worker.start("fixture-one".into(), url.clone());
        let deadline = Instant::now() + Duration::from_secs(20);
        let first = loop {
            assert!(Instant::now() < deadline, "no first thumbnail");
            if let Outbound::PreviewFrame { image, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break image;
            }
        };
        *CHANGE_ON_SEEK.lock().unwrap() = Some((worker.shared.clone(), Some(20.0)));
        worker.request("fixture-one", Some(10.1));
        let (later, position) = loop {
            assert!(Instant::now() < deadline, "no requested thumbnail");
            if let Outbound::PreviewFrame {
                image,
                bucket: 2,
                position,
                ..
            } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break (image, position);
            }
        };
        let expected = std::env::var("PREVIEW_TEST_EXPECT_LANDING")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(12.0);
        assert!(
            (position - expected).abs() < 1.0,
            "wrong sampled position: {position}"
        );
        loop {
            assert!(
                Instant::now() < deadline,
                "newest hover was lost while finishing old seek"
            );
            if let Outbound::PreviewFrame { bucket: 4, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break;
            }
        }
        assert_ne!(first, later, "distant positions have identical images");
        if let Ok(dir) = std::env::var("PREVIEW_TEST_OUTPUT") {
            for (name, data) in [("first.jpg", &first), ("later.jpg", &later)] {
                let bytes = STANDARD
                    .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap())
                    .unwrap();
                std::fs::write(Path::new(&dir).join(name), bytes).unwrap();
            }
        }
        worker.request("fixture-one", Some(0.0));
        worker.request("fixture-one", Some(10.0));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "latest requested position lost");
            if let Outbound::PreviewFrame { bucket: 2, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break;
            }
        }
        main.command(&["seek", "6", "absolute", "keyframes"])
            .unwrap();
        worker.request("fixture-one", Some(16.0));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "hover after a main-player seek stalled"
            );
            if let Outbound::PreviewFrame { bucket: 3, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break;
            }
        }
        assert_eq!(
            OPEN_COUNT.load(Ordering::Relaxed),
            1,
            "main-player seek reopened the decoder"
        );
        worker.stop("fixture-one");
        while receive.try_recv().is_ok() {}
        worker.request("fixture-one", Some(10.0));
        assert!(
            receive.recv_timeout(Duration::from_millis(500)).is_err(),
            "stopped session emitted frames"
        );
        drop(worker);
        // Even clearing the hover after a seek must finish and retain the image.
        let session = shared.0.lock().unwrap().session.clone().unwrap();
        let direct = open(&library, &main, &session, &shared, 1, false).unwrap();
        shared.0.lock().unwrap().session.as_mut().unwrap().wanted = Some(10.1);
        *CHANGE_ON_SEEK.lock().unwrap() = Some((shared.clone(), None));
        let completed = capture(
            &direct,
            &main,
            &session,
            &shared,
            1,
            Capture {
                target: 2,
                demand: true,
                step: 5.0,
                reopened: false,
            },
        )
        .expect("clearing the hover discarded a paid-for seek");
        assert_eq!(completed.covers_until, 12.5);
        let cache = VecDeque::from([completed]);
        assert!(nearby(&cache, 10.1, 2.5).is_some());
        assert!(nearby(&cache, 15.1, 2.5).is_none());
        // Source stop still cancels bounded work and prevents delivery.
        shared.0.lock().unwrap().generation += 1;
        assert_eq!(
            wait_frame(&direct.mpv, &shared, 1, Duration::from_secs(8)),
            Err("cancelled")
        );
        shared.0.lock().unwrap().generation = 1;
        if std::env::var("PREVIEW_TEST_EXPECT_GAMMA").as_deref() == Ok("pq") {
            // Compare the default HDR pipeline with the explicit known-good filter.
            let baseline_session = shared.0.lock().unwrap().session.clone().unwrap();
            let baseline = open(&library, &main, &baseline_session, &shared, 1, false).unwrap();
            baseline.mpv.set_property("vf", "lavfi=[zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,scale=320:-2,format=yuv420p]").unwrap();
            let image = capture(
                &baseline,
                &main,
                &baseline_session,
                &shared,
                1,
                Capture {
                    target: 0,
                    demand: false,
                    step: 5.0,
                    reopened: false,
                },
            )
            .unwrap();
            if let Ok(dir) = std::env::var("PREVIEW_TEST_OUTPUT") {
                std::fs::write(
                    Path::new(&dir).join("baseline.jpg"),
                    STANDARD
                        .decode(image.data.strip_prefix("data:image/jpeg;base64,").unwrap())
                        .unwrap(),
                )
                .unwrap();
            }
        }
    }
}
