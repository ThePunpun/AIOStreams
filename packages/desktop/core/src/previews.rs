//! Session-scoped seek images. A second, small decoder never seeks the playing decoder.

use std::collections::{BTreeSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::bridge::Outbound;
use crate::mpv::{Event, Kind, Mpv};
use crate::player::Emit;

const MAX_CAPTURES: usize = 1200;
const MAX_WORK_MS: u64 = 180_000;
const MAX_IMAGES: usize = 400;
const MAX_BACKGROUND_WORK_MS: u64 = 90_000;
const MAX_FAILED_WORK_MS: u64 = 90_000;

#[derive(Clone)]
struct Session {
    id: String,
    url: String,
    wanted: Option<f64>,
    pointer_on_timeline: bool,
    last_hover: Option<f64>,
    hover_direction: i32,
    hover_settled_at: Instant,
    hover_pending: bool,
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
            pointer_on_timeline: false,
            last_hover: None,
            hover_direction: 1,
            hover_settled_at: Instant::now(),
            hover_pending: true,
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
            s.hover_pending = s.wanted.is_none();
            if let Some(position) = s.wanted {
                if let Some(previous) = s.last_hover
                    && position != previous
                {
                    s.hover_direction = if position > previous { 1 } else { -1 };
                }
                s.last_hover = Some(position);
                s.hover_settled_at = Instant::now();
            }
            self.shared.1.notify_one();
        }
    }
    pub fn hover(&self, id: &str, active: bool) {
        let mut state = self.shared.0.lock().unwrap();
        if let Some(session) = state.session.as_mut().filter(|s| s.id == id) {
            session.pointer_on_timeline = active;
            if !active {
                session.wanted = None;
            }
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
    if duration.is_finite() && duration > 0.0 {
        ((duration / 600.0 / 5.0).round() * 5.0).clamp(5.0, 30.0)
    } else {
        10.0
    }
}

fn aspect_ratio(mpv: &Mpv, params: &str) -> Option<f64> {
    number(mpv, &format!("{params}/aspect"))
        .or_else(|| {
            number(mpv, &format!("{params}/dw"))
                .zip(number(mpv, &format!("{params}/dh")))
                .map(|(width, height)| width / height)
        })
        .filter(|ratio| (0.1..=10.0).contains(ratio))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkKind {
    OpenFrame,
    Demand,
    Background,
    Neighbor,
}

fn should_yield(session: &Session, kind: WorkKind) -> bool {
    match kind {
        WorkKind::Background => session.pointer_on_timeline || session.wanted.is_some(),
        WorkKind::Neighbor => {
            !session.pointer_on_timeline || session.wanted.is_some() || session.hover_pending
        }
        // Keep demand captures live on long-GOP sources, even during rapid movement.
        WorkKind::OpenFrame | WorkKind::Demand => false,
    }
}

fn background_allowed(calm: bool, buffered: Option<f64>, pointer: bool) -> bool {
    calm && !pointer && buffered.is_some_and(|seconds| seconds >= 15.0)
}

fn median_seek(samples: &VecDeque<u64>) -> Option<u64> {
    if samples.len() < 3 {
        return None;
    }
    let mut sorted: Vec<_> = samples.iter().copied().collect();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
}

// The software thumbnail path cannot apply Dolby Vision's reshaping metadata.
// Permit known compatible base layers; fail closed for profile 5/20 or unknown DV.
fn dv_base_supported(profile: Option<u64>, matrix: Option<&str>) -> bool {
    match profile {
        Some(7..=9) => true,
        Some(_) => false,
        None => matrix != Some("dolbyvision"),
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

// A speculative seek yields to user work; a demand seek finishes and caches.
fn wait_frame(
    mpv: &Mpv,
    shared: &Shared,
    generation: u64,
    timeout: Duration,
) -> Result<(), &'static str> {
    wait_frame_for(
        mpv,
        shared,
        generation,
        timeout,
        WorkKind::Demand,
        None,
        false,
    )
}

fn wait_frame_for(
    mpv: &Mpv,
    shared: &Shared,
    generation: u64,
    timeout: Duration,
    kind: WorkKind,
    main: Option<&Mpv>,
    require_seek: bool,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    let slow_at = Instant::now() + Duration::from_secs(2);
    let mut logged_slow = false;
    let mut saw_seek = !require_seek;
    while Instant::now() < deadline {
        if !active(shared, generation) {
            return Err("cancelled");
        }
        if saw_seek
            && shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .is_some_and(|s| should_yield(s, kind))
        {
            return Err("superseded-background");
        }
        if saw_seek
            && matches!(kind, WorkKind::Background | WorkKind::Neighbor)
            && main.is_some_and(|m| {
                m.get_property("paused-for-cache", Kind::Flag) == Some(true.into())
            })
        {
            return Err("playback-buffering");
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
            Some(Event::Seek) => saw_seek = true,
            Some(Event::PlaybackRestart)
                if saw_seek && mpv.get_property("seeking", Kind::Flag) != Some(true.into()) =>
            {
                return Ok(());
            }
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
    covers_from: f64,
    data: String,
    aspect_ratio: f64,
    seek_ms: u64,
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
    background_work_ms: u64,
    preempted: usize,
    neighbors: usize,
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
        Ok("zscale-first") => "zscale-first",
        Ok("skip-loop-filter") => "skip-loop-filter",
        Ok("threads-4") => "threads-4",
        Ok("persistent-http") => "persistent-http",
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
        (
            "vd-lavc-threads",
            if variant == "threads-4" { "4" } else { "2" },
        ),
        ("hwdec", "no"),
        ("vf", "scale=320:-2"),
        ("screenshot-format", "jpg"),
        ("screenshot-jpeg-quality", "70"),
        ("hr-seek", "no"),
    ];
    match variant {
        "skip-loop-filter" => options.push(("vd-lavc-skiploopfilter", "all")),
        "persistent-http" => options.push(("stream-lavf-o", "multiple_requests=1")),
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
    let input = decoder.mpv.get_property("video-dec-params", Kind::Json);
    let gamma = input
        .as_ref()
        .and_then(|v| v.get("gamma"))
        .and_then(|v| v.as_str());
    let matrix = input
        .as_ref()
        .and_then(|v| v.get("colormatrix"))
        .and_then(|v| v.as_str());
    let profile = decoder
        .mpv
        .get_property("current-tracks/video/dolby-vision-profile", Kind::Json)
        .and_then(|v| v.as_u64())
        .or_else(|| {
            let tracks = decoder.mpv.get_property("track-list", Kind::Json)?;
            tracks
                .as_array()?
                .iter()
                .find(|track| {
                    track.get("type").and_then(|v| v.as_str()) == Some("video")
                        && track.get("selected").and_then(|v| v.as_bool()) == Some(true)
                })?
                .get("dolby-vision-profile")?
                .as_u64()
        });
    if !dv_base_supported(profile, matrix) {
        log::info!(target: "seek_preview", "session={} colour_path=dolby-vision-unavailable dv_profile={profile:?}", session.id);
        return Err("dolby-vision-needs-conversion");
    }
    let mut base_gamma = gamma.map(str::to_owned);
    if profile.is_some() {
        let prepare_at = Instant::now();
        decoder
            .mpv
            .set_property("vf", "format=dolbyvision=no,scale=320:-2")
            .map_err(|_| "dv-base-filter")?;
        while decoder.mpv.wait_event(0.0, |_| false).is_some() {}
        let position = number(&decoder.mpv, "time-pos").ok_or("no-position")?;
        decoder
            .mpv
            .command(&["seek", &position.to_string(), "absolute", "keyframes"])
            .map_err(|_| "dv-base-seek")?;
        // Ignore a filter-restart event until this seek has actually started.
        wait_frame_for(
            &decoder.mpv,
            shared,
            generation,
            Duration::from_secs(8),
            WorkKind::Demand,
            None,
            true,
        )?;
        base_gamma = decoder
            .mpv
            .get_property("video-out-params", Kind::Json)
            .and_then(|v| v.get("gamma").and_then(|v| v.as_str()).map(str::to_owned));
        if base_gamma.is_none() {
            return Err("dv-base-colour-unavailable");
        }
        log::info!(target: "seek_preview", "session={} dv_base_prepare_ms={} dv_profile={profile:?} base_transfer={}", session.id, prepare_at.elapsed().as_millis(), match base_gamma.as_deref() { Some("pq") => "pq", Some("hlg") => "hlg", _ => "sdr" });
    }
    let gamma = base_gamma.as_deref();
    let hdr = matches!(gamma, Some("pq" | "hlg"));
    let filter_name = if hdr {
        match variant {
            "scale-first" => "scale-first",
            "zscale-first" => "zscale-first",
            _ => "tonemap-first-v1",
        }
    } else {
        "scale-only"
    };
    let filter = if hdr {
        match variant {
            "scale-first" => {
                "lavfi=[scale=320:-2:flags=area,zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,format=yuv420p]"
            }
            "zscale-first" => {
                "lavfi=[zscale=w=320:h=-2:transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,format=yuv420p]"
            }
            _ => {
                "lavfi=[zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,scale=320:-2,format=yuv420p]"
            }
        }
    } else {
        "scale=320:-2"
    };
    // Restore original base-layer colour tags before software filtering DV-compatible files.
    let filter = if profile.is_some() {
        format!("format=dolbyvision=no,{filter}")
    } else {
        filter.to_owned()
    };
    if hdr || profile.is_some() {
        decoder
            .mpv
            .set_property("vf", &filter)
            .map_err(|_| "hdr-filter")?;
        if hdr {
            // Changing vf can queue an asynchronous refresh seek. The already-open
            // frame must actually pass through the SDR filter before it is cached.
            // Do not issue another remote seek just to warm up the screenshot file.
            let filter_at = Instant::now();
            let mut logged_slow = false;
            loop {
                if !active(shared, generation) {
                    return Err("cancelled");
                }
                let transfer = decoder
                    .mpv
                    .get_property("video-out-params/gamma", Kind::String);
                if transfer
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| matches!(v, "bt.1886" | "srgb" | "gamma2.2" | "gamma2.4"))
                    && decoder.mpv.get_property("seeking", Kind::Flag) != Some(true.into())
                {
                    break;
                }
                if !logged_slow && filter_at.elapsed() >= Duration::from_secs(2) {
                    logged_slow = true;
                    log_decoder_state(&decoder.mpv, &session.id, "waiting-hdr-filter-over-2s");
                }
                if filter_at.elapsed() >= Duration::from_secs(8) {
                    return Err("hdr-filter-not-ready");
                }
                if let Some(
                    Event::EndFile {
                        reason: "error", ..
                    }
                    | Event::Shutdown,
                ) = decoder.mpv.wait_event(0.05, |_| false)
                {
                    return Err("decode-error");
                }
            }
            log::info!(target: "seek_preview", "session={} hdr_filter_ready_ms={}", session.id, filter_at.elapsed().as_millis());
        }
    }
    let colour_path = if profile.is_some() {
        "dolby-vision-base-layer"
    } else if gamma == Some("pq") {
        "pq-hdr10-base"
    } else if gamma == Some("hlg") {
        "hlg"
    } else {
        "sdr"
    };
    log::info!(target: "seek_preview", "session={} opened_ms={} reopened={reopened} hdr={hdr} filter={filter_name} threads={} test_variant={variant} colour_path={colour_path} dv_profile={profile:?}", session.id, start.elapsed().as_millis(), if variant == "threads-4" { 4 } else { 2 });
    log_decoder_state(&decoder.mpv, &session.id, "opened");
    Ok(decoder)
}

struct Capture {
    target: u32,
    kind: WorkKind,
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
        kind,
        step,
        reopened,
    } = request;
    let demand = kind == WorkKind::Demand;
    let open_frame = kind == WorkKind::OpenFrame;
    // Drain old restart events so an earlier seek cannot complete this request.
    while decoder.mpv.wait_event(0.0, |_| false).is_some() {}
    let wanted_s = if open_frame {
        number(&decoder.mpv, "time-pos").ok_or("no-position")?
    } else {
        ((f64::from(target) + 0.5) * step)
            .min((number(main, "duration").unwrap_or(86400.0) - 0.05).max(0.0))
    };
    let playhead = number(main, "time-pos");
    let main_buffered = in_buffered_range(main, wanted_s);
    let started = Instant::now();
    if !open_frame {
        decoder
            .mpv
            .command(&["seek", &wanted_s.to_string(), "absolute", "keyframes"])
            .map_err(|_| "seek-command")?;
        log::info!(target: "seek_preview", "session={} bucket={target} seek_issued=true demand={demand} target_s={wanted_s:.3}", session.id);
        wait_frame_for(
            &decoder.mpv,
            shared,
            generation,
            Duration::from_secs(8),
            kind,
            Some(main),
            true,
        )?;
    }
    if !active(shared, generation) {
        return Err("cancelled");
    }
    if !open_frame
        && shared
            .0
            .lock()
            .unwrap()
            .session
            .as_ref()
            .is_some_and(|s| should_yield(s, kind))
    {
        return Err("superseded-background");
    }
    let seek_ms = if open_frame {
        0
    } else {
        started.elapsed().as_millis() as u64
    };
    let position = number(&decoder.mpv, "time-pos").ok_or("no-position")?;
    let path = decoder.dir.join("frame.jpg");
    let screenshot_at = Instant::now();
    decoder
        .mpv
        .command(&["screenshot-to-file", &path.to_string_lossy(), "video"])
        .map_err(|_| "screenshot")?;
    let screenshot_ms = screenshot_at.elapsed().as_millis();
    let read_at = Instant::now();
    let open_at = Instant::now();
    let mut file = std::fs::File::open(&path).map_err(|_| "open-image")?;
    let file_open_ms = open_at.elapsed().as_micros() as f64 / 1000.0;
    let bytes_at = Instant::now();
    let mut bytes = Vec::new();
    file.by_ref()
        .take(512 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "read-image")?;
    let file_read_ms = bytes_at.elapsed().as_micros() as f64 / 1000.0;
    let close_at = Instant::now();
    drop(file);
    let file_close_ms = close_at.elapsed().as_micros() as f64 / 1000.0;
    let delete_at = Instant::now();
    let _ = std::fs::remove_file(&path);
    let file_delete_ms = delete_at.elapsed().as_micros() as f64 / 1000.0;
    if file_open_ms
        .max(file_read_ms)
        .max(file_close_ms)
        .max(file_delete_ms)
        >= 250.0
    {
        log::info!(target: "seek_preview", "session={} slow_image_io=true open_frame={open_frame} file_open_ms={file_open_ms:.3} file_read_ms={file_read_ms:.3} file_close_ms={file_close_ms:.3} file_delete_ms={file_delete_ms:.3} image_bytes={} cause=unverified", session.id, bytes.len());
    }
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
    let cache_state = decoder.mpv.get_property("demuxer-cache-state", Kind::Json);
    let queue_total = cache_state
        .as_ref()
        .and_then(|v| v.get("total-bytes"))
        .and_then(|v| v.as_u64());
    let queue_forward = cache_state
        .as_ref()
        .and_then(|v| v.get("fw-bytes"))
        .and_then(|v| v.as_u64());
    let raw_rate = cache_state
        .as_ref()
        .and_then(|v| v.get("raw-input-rate"))
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite());
    let aspect = aspect_ratio(&decoder.mpv, "video-out-params").unwrap_or(16.0 / 9.0);
    let transfer = match output
        .as_ref()
        .and_then(|v| v.get("gamma"))
        .and_then(|v| v.as_str())
    {
        Some("pq") => "pq",
        Some("hlg") => "hlg",
        Some("bt.1886" | "srgb" | "gamma2.2" | "gamma2.4") => "sdr",
        _ => "unknown",
    };
    log::info!(target: "seek_preview", "session={} bucket={} target_s={wanted_s:.3} sampled_s={position:.3} covers_from_s={:.3} covers_until_s={:.3} offset_s={:.3} demand={} seek_ms={seek_ms} capture_ms={} screenshot_ms={screenshot_ms} read_ms={read_ms:.3} file_open_ms={file_open_ms:.3} file_read_ms={file_read_ms:.3} file_close_ms={file_close_ms:.3} file_delete_ms={file_delete_ms:.3} base64_ms={base64_ms:.3} image_bytes={} playhead_distance_s={:?} main_buffered={main_buffered:?} reopened={reopened} vf_width={width:?} vf_height={height:?} aspect_ratio={aspect:.5} output_transfer={transfer} open_frame={open_frame} demux_queue_total_bytes={queue_total:?} demux_queue_forward_bytes={queue_forward:?} raw_input_bytes_per_second={raw_rate:?}", session.id, target, wanted_s.min(position), wanted_s.max(position), position - wanted_s, demand, screenshot_at.elapsed().as_millis(), bytes.len(), playhead.map(|v| wanted_s-v));
    Ok(Image {
        bucket: target,
        position,
        covers_until: wanted_s.max(position),
        covers_from: wanted_s.min(position),
        data,
        aspect_ratio: aspect,
        seek_ms,
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
        .filter(|i| seconds >= i.covers_from - tolerance && seconds <= i.covers_until + tolerance)
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

fn keep_image(cache: &mut VecDeque<Image>, image: Image) {
    cache.retain(|old| old.bucket != image.bucket);
    cache.push_back(image);
    while cache.len() > MAX_IMAGES
        || cache.iter().map(|i| i.data.len()).sum::<usize>() > 8 * 1024 * 1024
    {
        cache.pop_front();
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
        let mut visited = BTreeSet::new();
        let mut recent_seeks = VecDeque::new();
        let mut neighbor_done = None;
        let mut decoder = None;
        let mut healthy_since = None;
        let mut background_at = Instant::now();
        let mut retry_at = Instant::now();
        let mut last_work = Instant::now();
        let mut backoff = Duration::from_millis(250);
        let mut was_buffering = false;
        let mut failed = false;
        let mut consecutive_errors = 0;
        let mut status = "";
        let mut background_status = "";
        let mut step = 10.0;
        let mut configured = false;
        let mut fresh_open = false;
        let mut sent_aspect = None;
        log::info!(target: "seek_preview", "session={} start cached_debrid=true max_background_work_ms={MAX_BACKGROUND_WORK_MS} max_completed_captures={MAX_CAPTURES} max_work_ms={MAX_WORK_MS} max_images={MAX_IMAGES} cache_encoded_limit_bytes=8388608 min_background_buffer_s=15 idle_close_s=90", session.id);
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
                log::info!(target: "seek_preview", "session={} step_ms={} duration_s={duration:.3}", session.id, (step * 1000.0) as u32);
            }
            let next_status = if failed {
                "unavailable"
            } else if !hover_ok {
                "waiting"
            } else {
                "ready"
            };
            let aspect = aspect_ratio(main, "video-params");
            if status != next_status || aspect != sent_aspect {
                if status != next_status && next_status == "waiting" {
                    stats.waiting += 1;
                }
                status = next_status;
                sent_aspect = aspect;
                emit(Outbound::PreviewStatus {
                    session: session.id.clone(),
                    state: status.into(),
                    step_ms: (step * 1000.0) as u32,
                    aspect_ratio: aspect,
                });
                log::info!(target: "seek_preview", "session={} state={status} decoder_open={} aspect_ratio={aspect:?}", session.id, decoder.is_some());
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
            let buffering = main.get_property("paused-for-cache", Kind::Flag) == Some(true.into());
            if buffering && !was_buffering {
                backoff = (backoff * 2).clamp(Duration::from_secs(1), Duration::from_secs(10));
                background_at = Instant::now() + backoff;
                log::info!(target: "seek_preview", "session={} playback_buffering=true background_backoff_ms={}", session.id, backoff.as_millis());
            }
            was_buffering = buffering;
            let current = {
                let state = shared.0.lock().unwrap();
                if state.generation != generation || state.shutdown {
                    break;
                }
                let Some(session) = state.session.clone() else {
                    break;
                };
                session
            };
            let buffered = number(main, "demuxer-cache-duration");
            let background_ok = background_allowed(calm, buffered, current.pointer_on_timeline);
            let bg_state = if stats.background_work_ms >= MAX_BACKGROUND_WORK_MS {
                "budget"
            } else if current.pointer_on_timeline {
                "pointer"
            } else if !calm {
                "playback-busy"
            } else if !background_ok {
                "buffer-low-or-unknown"
            } else {
                "ready"
            };
            if bg_state != background_status {
                background_status = bg_state;
                log::info!(target: "seek_preview", "session={} background_state={bg_state} main_buffer_ahead_s={buffered:?} background_work_ms={}", session.id, stats.background_work_ms);
            }
            if !hover_ok || failed || !configured {
                std::thread::park_timeout(Duration::from_millis(100));
                continue;
            }
            let wanted_seconds = current.wanted;
            let wanted = wanted_seconds.and_then(|seconds| bucket(seconds, step));
            let mut kind = WorkKind::Demand;
            let target = wanted.or_else(|| {
                if stats.background_work_ms >= MAX_BACKGROUND_WORK_MS
                    || Instant::now() < background_at
                {
                    return None;
                }
                if current.pointer_on_timeline {
                    // One neighbor only, after a fast hover finishes and rests for 350 ms.
                    if !calm
                        || current.hover_pending
                        || buffered.is_none_or(|v| v < 15.0)
                        || median_seek(&recent_seeks).is_none_or(|ms| ms >= 500)
                        || current.hover_settled_at.elapsed() < Duration::from_millis(350)
                    {
                        return None;
                    }
                    let position = current.last_hover?;
                    let origin = bucket(position, step)?;
                    if neighbor_done == Some((origin, current.hover_direction)) {
                        return None;
                    }
                    neighbor_done = Some((origin, current.hover_direction));
                    let next = i64::from(origin) + i64::from(current.hover_direction);
                    let duration = number(main, "duration")?;
                    if next < 0 || (next as f64 + 0.5) * step >= duration {
                        return None;
                    }
                    let next = next as u32;
                    if nearby(&cache, (f64::from(next) + 0.5) * step, step / 2.0).is_some() {
                        return None;
                    }
                    kind = WorkKind::Neighbor;
                    return Some(next);
                }
                if !background_ok {
                    return None;
                }
                let position = number(main, "time-pos")?;
                let duration = number(main, "duration")?;
                // Search the finite grid from the current playhead, skipping cached/visited
                // positions. A clamped end bucket can never create a busy loop or repeat work.
                let candidates = ((duration.clamp(0.0, 86400.0) / step).ceil() as usize + 1) * 5;
                for index in 0..candidates {
                    let next = background_target(position, duration, step, index)?;
                    if visited.contains(&next) {
                        continue;
                    }
                    if nearby(&cache, (f64::from(next) + 0.5) * step, step / 2.0).is_some() {
                        visited.insert(next);
                        continue;
                    }
                    kind = WorkKind::Background;
                    return Some(next);
                }
                None
            });
            let Some(target) = target else {
                let state = shared.0.lock().unwrap();
                let _ = shared
                    .1
                    .wait_timeout(state, Duration::from_millis(100))
                    .unwrap();
                continue;
            };
            let demand = kind == WorkKind::Demand;
            if let Some(image) = nearby(
                &cache,
                wanted_seconds.unwrap_or((f64::from(target) + 0.5) * step),
                step / 2.0,
            ) {
                if demand {
                    stats.hits += 1;
                    deliver(emit, &session, image, true, 0);
                    finish_request(shared, generation, target, step);
                }
                continue;
            }
            if stats.generated >= MAX_CAPTURES
                || stats.work_ms >= MAX_WORK_MS
                || stats.failed_work_ms >= MAX_FAILED_WORK_MS
            {
                failed = true;
                decoder = None;
                log::info!(target: "seek_preview", "session={} budget_exhausted=true generated={} work_ms={} failed_work_ms={}", session.id, stats.generated, stats.work_ms, stats.failed_work_ms);
                continue;
            }
            if decoder.is_none() {
                if stats.opens == 0
                    && healthy_since.is_none_or(|t| t.elapsed() < Duration::from_secs(1))
                {
                    std::thread::park_timeout(Duration::from_millis(100));
                    continue;
                }
                match open(library, main, &session, shared, generation, stats.opens > 0) {
                    Ok(d) => {
                        stats.opens += 1;
                        fresh_open = stats.opens > 1;
                        let opened_at = Instant::now();
                        let position = number(&d.mpv, "time-pos").unwrap_or(0.0);
                        // Cache the frame already decoded at open. This is not a new seek.
                        match capture(
                            &d,
                            main,
                            &session,
                            shared,
                            generation,
                            Capture {
                                target: bucket(position, step).unwrap_or(0),
                                kind: WorkKind::OpenFrame,
                                step,
                                reopened: fresh_open,
                            },
                        ) {
                            Ok(image) if active(shared, generation) => {
                                stats.generated += 1;
                                stats.work_ms += opened_at.elapsed().as_millis() as u64;
                                deliver(
                                    emit,
                                    &session,
                                    &image,
                                    false,
                                    opened_at.elapsed().as_millis() as u64,
                                );
                                keep_image(&mut cache, image);
                            }
                            Ok(_) | Err("cancelled") => break,
                            Err(reason) => {
                                // A warm-up screenshot failure does not disable real demand work.
                                log::info!(target: "seek_preview", "session={} open_frame_failed={reason}", session.id);
                                stats.failed_work_ms += opened_at.elapsed().as_millis() as u64;
                            }
                        }
                        decoder = Some(d);
                        last_work = Instant::now();
                    }
                    Err("cancelled") => break,
                    Err(reason) => {
                        log::info!(target: "seek_preview", "session={} open_failed={reason}", session.id);
                        stats.errors += 1;
                        failed = true;
                    }
                }
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
                    kind,
                    step,
                    reopened: fresh_open,
                },
            );
            let elapsed = started.elapsed().as_millis() as u64;
            if !demand {
                stats.background_work_ms += elapsed;
            }
            if let Err(reason) = result.as_ref() {
                // Expected speculative interruptions already spend the background budget.
                // They must not disable later demand work through the failure guard.
                if !matches!(
                    *reason,
                    "cancelled" | "superseded-background" | "playback-buffering"
                ) {
                    stats.failed_work_ms += elapsed;
                }
                log::info!(target: "seek_preview", "session={} bucket={target} demand={demand} aborted={reason} elapsed_ms={elapsed}", session.id);
                log_decoder_state(&decoder.as_ref().unwrap().mpv, &session.id, reason);
            }
            let completed = result.is_ok();
            match result {
                Ok(image) => {
                    if !active(shared, generation) {
                        break;
                    }
                    stats.generated += 1;
                    stats.work_ms += elapsed;
                    consecutive_errors = 0;
                    recent_seeks.push_back(image.seek_ms);
                    if recent_seeks.len() > 7 {
                        recent_seeks.pop_front();
                    }
                    if !demand {
                        visited.insert(target);
                    }
                    if kind == WorkKind::Neighbor {
                        stats.neighbors += 1;
                    }
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
                    deliver(emit, &session, &image, false, elapsed);
                    keep_image(&mut cache, image);
                    backoff = Duration::from_millis((elapsed / 2).clamp(100, 1500));
                    fresh_open = false;
                }
                Err("cancelled") => break,
                Err("superseded-background" | "playback-buffering") => {
                    stats.preempted += 1;
                    // Expected speculative cancellation is not a decoder error.
                }
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
        log::info!(target: "seek_preview", "session={} summary generated={} background_candidates={} background_work_ms={} neighbors={} preempted={} native_hits={} errors={} completed_after_move={} opens={} waiting_intervals={} attempts={} work_ms={} failed_work_ms={} network_bytes=unavailable", session.id, stats.generated, stats.background, stats.background_work_ms, stats.neighbors, stats.preempted, stats.hits, stats.errors, stats.completed_after_move, stats.opens, stats.waiting, stats.attempts, stats.work_ms, stats.failed_work_ms);
    }
}

fn deliver(emit: &Emit, session: &Session, image: &Image, cached: bool, elapsed_ms: u64) {
    emit(Outbound::PreviewFrame {
        session: session.id.clone(),
        bucket: image.bucket,
        position: image.position,
        covers_until: image.covers_until,
        covers_from: image.covers_from,
        image: image.data.clone(),
        aspect_ratio: image.aspect_ratio,
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
    static CHANGE_ON_BACKGROUND: Mutex<Option<Shared>> = Mutex::new(None);
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
                if message.contains("seek_issued=true demand=false")
                    && let Some(shared) = CHANGE_ON_BACKGROUND.lock().unwrap().take()
                    && let Some(session) = shared.0.lock().unwrap().session.as_mut()
                {
                    session.pointer_on_timeline = true;
                    session.wanted = Some(10.1);
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
        assert_eq!(step_for(1800.0), 5.0);
        assert_eq!(step_for(7200.0), 10.0);
        assert_eq!(step_for(15000.0), 25.0);
        assert_eq!(step_for(86400.0), 30.0);
        assert_eq!(step_for(0.0), 10.0);
        assert_eq!(background_target(60.0, 3600.0, 10.0, 1), Some(7));
        assert_eq!(background_target(60.0, 3600.0, 10.0, 5), Some(5));
        assert_eq!(background_target(0.0, 3600.0, 10.0, 5), Some(0));
        let cache = VecDeque::from([Image {
            bucket: 2,
            position: 7.0,
            covers_until: 25.0,
            covers_from: 7.0,
            data: String::new(),
            aspect_ratio: 16.0 / 9.0,
            seek_ms: 0,
        }]);
        assert!(nearby(&cache, 29.9, 5.0).is_some());
        assert!(nearby(&cache, 30.1, 5.0).is_none());
        assert!(nearby(&cache, 1.9, 5.0).is_none());
    }

    #[test]
    fn protects_playback_and_demand_while_speculation_yields() {
        let mut session = Session {
            id: "policy".into(),
            url: "fixture".into(),
            wanted: None,
            pointer_on_timeline: false,
            last_hover: None,
            hover_direction: 1,
            hover_settled_at: Instant::now(),
            hover_pending: true,
        };
        assert!(!should_yield(&session, WorkKind::Background));
        session.pointer_on_timeline = true;
        session.hover_pending = false;
        assert!(should_yield(&session, WorkKind::Background));
        assert!(!should_yield(&session, WorkKind::Neighbor));
        session.wanted = Some(10.0);
        assert!(should_yield(&session, WorkKind::Neighbor));
        assert!(!should_yield(&session, WorkKind::Demand));
        assert!(!background_allowed(true, None, false));
        assert!(!background_allowed(true, Some(14.9), false));
        assert!(!background_allowed(false, Some(30.0), false));
        assert!(!background_allowed(true, Some(30.0), true));
        assert!(background_allowed(true, Some(15.0), false));
        assert_eq!(
            median_seek(&VecDeque::from([200, 1200, 250, 300, 275])),
            Some(275)
        );
        assert_eq!(median_seek(&VecDeque::from([200, 250])), None);
    }

    #[test]
    fn refuses_dv_profiles_that_need_reshaping_instead_of_showing_wrong_colours() {
        assert!(!dv_base_supported(Some(5), Some("dolbyvision")));
        assert!(!dv_base_supported(Some(20), Some("dolbyvision")));
        assert!(!dv_base_supported(None, Some("dolbyvision")));
        assert!(dv_base_supported(Some(7), Some("dolbyvision")));
        assert!(dv_base_supported(Some(8), Some("dolbyvision")));
        assert!(dv_base_supported(None, Some("bt.2020-ncl")));
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
                pointer_on_timeline: false,
                last_hover: None,
                hover_direction: 1,
                hover_settled_at: Instant::now(),
                hover_pending: true,
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
        let deadline = Instant::now() + Duration::from_secs(30);
        let first = loop {
            assert!(Instant::now() < deadline, "no first thumbnail");
            if let Outbound::PreviewFrame { image, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break image;
            }
        };
        // Exercise the production background scheduler before entering the timeline.
        loop {
            assert!(
                Instant::now() < deadline,
                "background prefetch did not run with healthy buffering"
            );
            if let Outbound::PreviewFrame { bucket: 1, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break;
            }
        }
        worker.hover("fixture-one", true);
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
                kind: WorkKind::Demand,
                step: 5.0,
                reopened: false,
            },
        )
        .expect("clearing the hover discarded a paid-for seek");
        assert_eq!(completed.covers_until, 12.5);
        let cache = VecDeque::from([completed]);
        assert!(nearby(&cache, 10.1, 2.5).is_some());
        assert!(nearby(&cache, 15.1, 2.5).is_none());
        // Interrupt an actual speculative seek after issuance, then prove demand recovers
        // on the same decoder instead of consuming a stale PlaybackRestart event.
        {
            let mut state = shared.0.lock().unwrap();
            let session = state.session.as_mut().unwrap();
            session.pointer_on_timeline = false;
            session.wanted = None;
        }
        *CHANGE_ON_BACKGROUND.lock().unwrap() = Some(shared.clone());
        let background = capture(
            &direct,
            &main,
            &session,
            &shared,
            1,
            Capture {
                target: 4,
                kind: WorkKind::Background,
                step: 5.0,
                reopened: false,
            },
        );
        assert!(matches!(background, Err("superseded-background")));
        let recovered = capture(
            &direct,
            &main,
            &session,
            &shared,
            1,
            Capture {
                target: 2,
                kind: WorkKind::Demand,
                step: 5.0,
                reopened: false,
            },
        )
        .unwrap();
        // A cache-assisted keyframe seek may land on either adjacent boundary.
        // Reject the old distant seek, and ensure a forward landing still covers the hover.
        assert!(
            (recovered.position - 12.5).abs() <= 2.5,
            "stale recovery: {}",
            recovered.position
        );
        assert!(nearby(&VecDeque::from([recovered]), 10.1, 2.5).is_some());
        // Source stop still cancels bounded work and prevents delivery.
        shared.0.lock().unwrap().generation += 1;
        assert_eq!(
            wait_frame(&direct.mpv, &shared, 1, Duration::from_secs(8)),
            Err("cancelled")
        );
        shared.0.lock().unwrap().generation = 1;
        if std::env::var("PREVIEW_TEST_EXPECT_GAMMA").as_deref() == Ok("pq") {
            // Compare the default HDR pipeline with the explicit known-good filter.
            while main.wait_event(0.0, |_| false).is_some() {}
            main.command(&["seek", "0", "absolute", "keyframes"])
                .unwrap();
            wait_frame_for(
                &main,
                &shared,
                1,
                Duration::from_secs(8),
                WorkKind::Demand,
                None,
                true,
            )
            .unwrap();
            let baseline_session = shared.0.lock().unwrap().session.clone().unwrap();
            let baseline = open(&library, &main, &baseline_session, &shared, 1, false).unwrap();
            baseline.mpv.set_property("vf", "lavfi=[zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,scale=320:-2,format=yuv420p]").unwrap();
            while baseline.mpv.wait_event(0.0, |_| false).is_some() {}
            baseline
                .mpv
                .command(&["seek", "0", "absolute", "keyframes"])
                .unwrap();
            wait_frame_for(
                &baseline.mpv,
                &shared,
                1,
                Duration::from_secs(8),
                WorkKind::Demand,
                None,
                true,
            )
            .unwrap();
            let image = capture(
                &baseline,
                &main,
                &baseline_session,
                &shared,
                1,
                Capture {
                    target: 0,
                    kind: WorkKind::OpenFrame,
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
