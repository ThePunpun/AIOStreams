//! Session-scoped seek images. A second, small decoder never seeks the playing decoder.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::bridge::{Outbound, PreviewColour, PreviewSource};
use crate::mpv::{Event, Kind, Mpv};
use crate::player::Emit;

const MAX_CAPTURES: usize = 17_282;
const MAX_DEMAND_WORK_MS: u64 = 180_000;
// Ample room for the duration-based grid, plus an opening frame.
// The encoded-size limit is usually reached first on very long files.
const MAX_IMAGES: usize = 8641;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BACKGROUND_WORK_MS: u64 = 7_200_000;
const MAX_FAILED_WORK_MS: u64 = 90_000;
const IDLE_CLOSE_S: u64 = 600;

#[derive(Clone)]
struct Session {
    id: String,
    url: String,
    source: PreviewSource,
    wanted: Option<f64>,
    pointer_on_timeline: bool,
    last_hover: Option<f64>,
    hover_direction: i32,
    hover_settled_at: Instant,
    hover_pending: bool,
    first_hover_at: Option<Instant>,
    first_display_reported: bool,
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
        self.start_with_source(id, url, PreviewSource::default());
    }

    pub fn start_with_source(&self, id: String, url: String, source: PreviewSource) {
        let mut state = self.shared.0.lock().unwrap();
        state.generation += 1;
        state.session = Some(Session {
            id,
            url,
            source,
            wanted: None,
            pointer_on_timeline: false,
            last_hover: None,
            hover_direction: 1,
            hover_settled_at: Instant::now(),
            hover_pending: true,
            first_hover_at: None,
            first_display_reported: false,
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
            if active {
                session.first_hover_at.get_or_insert_with(Instant::now);
            }
            if !active {
                session.wanted = None;
            }
            self.shared.1.notify_one();
        }
    }

    pub fn report_display(&self, id: &str, event: &str) {
        if !matches!(event, "display-cached" | "display-cold") {
            return;
        }
        let elapsed = {
            let mut state = self.shared.0.lock().unwrap();
            state
                .session
                .as_mut()
                .filter(|s| s.id == id && !s.first_display_reported)
                .and_then(|s| {
                    let at = s.first_hover_at?;
                    s.first_display_reported = true;
                    Some(at.elapsed().as_millis())
                })
        };
        if let Some(elapsed) = elapsed {
            log::info!(target: "seek_preview", "session={id} first_image_since_first_hover_ms={elapsed} display_kind={event}");
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
    // Retain the original duration-based density (roughly 600 slots for movies).
    // Hover and background work must use the same grid, including short episodes.
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

fn background_duty(playing: bool, buffered: Option<f64>, pointer: bool, reader_idle: bool) -> f64 {
    if !playing || pointer || buffered.is_none_or(|seconds| !seconds.is_finite() || seconds < 10.0)
    {
        0.0
    } else if reader_idle && buffered.is_some_and(|seconds| seconds >= 30.0) {
        0.5
    } else {
        0.25
    }
}

fn speculative_rest(elapsed_ms: u64, duty: f64) -> Duration {
    if duty <= 0.0 {
        return Duration::from_secs(10);
    }
    Duration::from_millis(((elapsed_ms as f64 * (1.0 / duty - 1.0)) as u64).max(100))
}

fn median_seek(samples: &VecDeque<u64>) -> Option<u64> {
    if samples.len() < 3 {
        return None;
    }
    let mut sorted: Vec<_> = samples.iter().copied().collect();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
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
        && main.get_property("video-out-params", Kind::Json).is_some()
        && main.get_property("seeking", Kind::Flag) != Some(true.into())
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
        && main.get_property("paused-for-cache", Kind::Flag) != Some(true.into())
        && number(main, "demuxer-cache-duration").is_none_or(|v| v == 0.0 || v >= 1.0)
}

// A speculative seek yields to user work; a demand seek finishes and caches.
struct WaitPolicy {
    kind: WorkKind,
    require_seek: bool,
    target: Option<u32>,
    step: f64,
    preempt_before_ms: Option<u64>,
}
impl WaitPolicy {
    fn new(kind: WorkKind, require_seek: bool) -> Self {
        Self {
            kind,
            require_seek,
            target: None,
            step: 10.0,
            preempt_before_ms: None,
        }
    }
}

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
        WaitPolicy::new(WorkKind::Demand, false),
        None,
    )
}

// Do not copy decoder text into preview logs: it can contain authenticated URLs.
fn filter_failure(prefix: &str, text: &str) -> Option<&'static str> {
    if !matches!(prefix, "ffmpeg" | "lavfi" | "vf") {
        return None;
    }
    let text = text.to_ascii_lowercase();
    if text.contains("libplacebo")
        && (text.contains("no such filter") || text.contains("not found"))
    {
        Some("libplacebo-missing")
    } else if text.contains("vulkan") && (text.contains("failed") || text.contains("vk_error")) {
        Some("vulkan-unavailable")
    } else if text.contains("apply_dolbyvision")
        && (text.contains("not found") || text.contains("error"))
    {
        Some("libplacebo-options-unavailable")
    } else if text.contains("failed to configure the filter graph")
        || text.contains("disabling filter")
    {
        Some("colour-filter-failed")
    } else {
        None
    }
}

fn wait_frame_for(
    mpv: &Mpv,
    shared: &Shared,
    generation: u64,
    timeout: Duration,
    policy: WaitPolicy,
    main: Option<&Mpv>,
) -> Result<(), &'static str> {
    let started = Instant::now();
    let deadline = started + timeout;
    let slow_at = Instant::now() + Duration::from_secs(2);
    let mut logged_slow = false;
    let mut saw_seek = !policy.require_seek;
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
                .is_some_and(|s| should_yield(s, policy.kind))
        {
            return Err("superseded-background");
        }
        if saw_seek
            && matches!(policy.kind, WorkKind::Background | WorkKind::Neighbor)
            && main.is_some_and(|m| {
                m.get_property("paused-for-cache", Kind::Flag) == Some(true.into())
                    || m.get_property("pause", Kind::Flag) == Some(true.into())
                    || m.get_property("seeking", Kind::Flag) == Some(true.into())
                    || number(m, "demuxer-cache-duration").is_none_or(|s| s < 10.0)
            })
        {
            return Err("playback-busy");
        }
        if saw_seek
            && policy.kind == WorkKind::Demand
            && policy
                .preempt_before_ms
                .is_some_and(|limit| started.elapsed().as_millis() < u128::from(limit))
            && mpv.get_property("seeking", Kind::Flag) == Some(true.into())
            && shared.0.lock().unwrap().session.as_ref().is_some_and(|s| {
                s.pointer_on_timeline
                    && s.wanted
                        .and_then(|v| bucket(v, policy.step))
                        .is_some_and(|wanted| Some(wanted) != policy.target)
            })
        {
            return Err("superseded-demand");
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
                let state = shared.0.lock().unwrap();
                if let Some(s) = state.session.as_ref()
                    && !s.first_display_reported
                    && let Some(at) = s.first_hover_at
                {
                    let underrun = mpv
                        .get_property("demuxer-cache-state", Kind::Json)
                        .and_then(|v| v.get("underrun").and_then(|v| v.as_bool()));
                    log::info!(target: "seek_preview", "session={session} first_hover_wait_reason=decoder-frame decoder_input_underrun={underrun:?} since_first_hover_ms={} network_cause=unverified", at.elapsed().as_millis());
                }
            }
        }
        match mpv.wait_event(0.05, |_| false) {
            Some(Event::Log { prefix, text, .. }) => {
                if let Some(reason) = filter_failure(&prefix, &text) {
                    return Err(reason);
                }
            }
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
    buffering_intervals: usize,
    background_work_ms: u64,
    preempted: usize,
    neighbors: usize,
    demand_work_ms: u64,
}

struct Decoder {
    mpv: Mpv,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.mpv.command(&["stop"]);
    }
}

pub fn test_variant() -> &'static str {
    match std::env::var("AIOSTREAMS_PREVIEW_TEST_VARIANT").as_deref() {
        Ok("scale-first") => "scale-first",
        Ok("zscale-first") => "zscale-first",
        Ok("tonemap-first" | "baseline") => "tonemap-first",
        Ok("dv-libplacebo") => "dv-libplacebo",
        Ok("skip-loop-filter") => "skip-loop-filter",
        Ok("threads-1") => "threads-1",
        Ok("threads-4") => "threads-4",
        Ok("persistent-http") => "persistent-http",
        Ok("small-back-cache") => "small-back-cache",
        Ok("mkv-no-duration") => "mkv-no-duration",
        _ => "default",
    }
}

fn video_profile(mpv: &Mpv) -> Option<u64> {
    mpv.get_property("current-tracks/video/dolby-vision-profile", Kind::Json)
        .and_then(|v| v.as_u64())
        .or_else(|| {
            let tracks = mpv.get_property("track-list", Kind::Json)?;
            tracks
                .as_array()?
                .iter()
                .find(|t| {
                    t.get("type").and_then(|v| v.as_str()) == Some("video")
                        && t.get("selected").and_then(|v| v.as_bool()) == Some(true)
                })?
                .get("dolby-vision-profile")?
                .as_u64()
        })
}

fn colour_plan(mpv: &Mpv, source: &PreviewSource) -> PreviewColour {
    use PreviewColour::*;
    let profile = video_profile(mpv).or(source.dv_profile);
    // Decoded identity overrides a stale or imprecise media-source hint.
    if profile == Some(5) {
        return Dv;
    }
    if profile.is_some_and(|p| !matches!(p, 7..=9)) {
        return Unknown;
    }
    if profile == Some(7) {
        return DvPq;
    }
    if profile.is_some() {
        return match source.colour {
            Pq | DvPq => DvPq,
            Hlg | DvHlg => DvHlg,
            Sdr | DvSdr => DvSdr,
            _ => Unknown,
        };
    }
    let params = mpv.get_property("video-dec-params", Kind::Json);
    let gamma = params
        .as_ref()
        .and_then(|v| v.get("gamma"))
        .and_then(|v| v.as_str());
    let matrix = params
        .as_ref()
        .and_then(|v| v.get("colormatrix"))
        .and_then(|v| v.as_str());
    if matrix != Some("dolbyvision") {
        match gamma {
            Some("pq") => return Pq,
            Some("hlg") => return Hlg,
            Some("bt.1886" | "srgb" | "gamma2.2" | "gamma2.4" | "linear") => return Sdr,
            _ => {}
        }
    }
    if source.colour != Unknown {
        return source.colour;
    }
    // For profile 8 without metadata, restore its base tags after opening before choosing PQ/HLG.
    if profile.is_some() || matrix == Some("dolbyvision") {
        return Unknown;
    }
    match gamma {
        Some("pq") => Pq,
        Some("hlg") => Hlg,
        Some("bt.1886" | "srgb" | "gamma2.2" | "gamma2.4" | "linear") => Sdr,
        _ => Unknown,
    }
}

fn colour_name(plan: PreviewColour) -> &'static str {
    use PreviewColour::*;
    match plan {
        Unknown => "auto",
        Sdr => "sdr",
        Pq => "pq-hdr10-base",
        Hlg => "hlg",
        DvPq => "dolby-vision-hdr10-base",
        DvHlg => "dolby-vision-hlg-base",
        DvSdr => "dolby-vision-sdr-base",
        Dv => "dolby-vision-libplacebo",
    }
}

fn preview_filter(plan: PreviewColour, variant: &str) -> String {
    use PreviewColour::*;
    if plan == Dv || variant == "dv-libplacebo" {
        return "lavfi=[libplacebo=w=320:h=-2:format=yuv420p:colorspace=bt709:color_primaries=bt709:color_trc=bt709:tonemapping=bt.2390:apply_dolbyvision=true]".into();
    }
    let filter = match plan {
        Pq | Hlg | DvPq | DvHlg => match variant {
            "tonemap-first" => {
                "lavfi=[zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,scale=320:-2,format=yuv420p]"
            }
            "zscale-first" => {
                "lavfi=[zscale=w=320:h=-2:transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,format=yuv420p]"
            }
            _ => {
                "lavfi=[scale=320:-2:flags=area,zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,format=yuv420p]"
            }
        },
        _ => "scale=320:-2",
    };
    if matches!(plan, DvPq | DvHlg | DvSdr) {
        format!("format=dolbyvision=no,{filter}")
    } else {
        filter.into()
    }
}

fn sdr_output(mpv: &Mpv) -> bool {
    mpv.get_property("video-out-params/gamma", Kind::String)
        .and_then(|v| v.as_str().map(str::to_owned))
        .is_some_and(|v| matches!(v.as_str(), "bt.1886" | "srgb" | "gamma2.2" | "gamma2.4"))
}

fn open(
    library: &Path,
    main: &Mpv,
    session: &Session,
    shared: &Shared,
    generation: u64,
    reopened: bool,
) -> Result<Decoder, &'static str> {
    let variant = test_variant();
    let mut plan = colour_plan(main, &session.source);
    let filter = preview_filter(plan, variant);
    let threads = match variant {
        "threads-1" => "1",
        "threads-4" => "4",
        _ => "2",
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
        ("vd-lavc-threads", threads),
        ("hwdec", "no"),
        ("vf", &filter),
        ("hr-seek", "no"),
    ];
    match variant {
        "skip-loop-filter" => options.push(("vd-lavc-skiploopfilter", "all")),
        "persistent-http" => options.push(("stream-lavf-o", "multiple_requests=1")),
        "small-back-cache" => options.push(("demuxer-max-back-bytes", "512KiB")),
        "mkv-no-duration" => options.push(("demuxer-mkv-probe-video-duration", "no")),
        _ => {}
    }
    let start = Instant::now();
    let mpv = Mpv::new(library, &options).map_err(|_| {
        if plan == PreviewColour::Dv || variant == "dv-libplacebo" {
            log::info!(target: "seek_preview", "session={} p5_filter=failed:decoder-init libplacebo_usable=false vulkan_initialised=unknown", session.id);
        }
        "decoder-init"
    })?;
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
            let _ = mpv.set_property(prop, &value);
        }
    }
    let position = number(main, "time-pos").unwrap_or(0.0);
    let _ = mpv.set_property("start", &position.to_string());
    let decoder = Decoder { mpv };
    let mut p5 = plan == PreviewColour::Dv || variant == "dv-libplacebo";
    let _ = decoder
        .mpv
        .request_log_messages(if p5 { "v" } else { "error" });
    log::info!(target: "seek_preview", "session={} opening=true test_variant={variant} colour_path={} filter_preconfigured={} dv_profile={:?} screenshot_path=memory", session.id, colour_name(plan), plan != PreviewColour::Unknown, video_profile(main).or(session.source.dv_profile));
    if p5 {
        log::info!(target: "seek_preview", "session={} p5_filter=attempting filter=libplacebo libplacebo_available=unknown vulkan_initialised=unknown dv_metadata_delivery=unverified", session.id);
    }
    decoder
        .mpv
        .command(&["loadfile", &session.url])
        .map_err(|_| "open-command")?;
    if let Err(reason) = wait_frame(
        &decoder.mpv,
        shared,
        generation,
        Duration::from_secs(if p5 { 3 } else { 12 }),
    ) {
        if p5 {
            let vulkan = if reason == "vulkan-unavailable" {
                "false"
            } else {
                "unknown"
            };
            let available = match reason {
                "vulkan-unavailable" => "true",
                "libplacebo-missing" => "false",
                _ => "unknown",
            };
            log::info!(target: "seek_preview", "session={} p5_filter=failed:{reason} libplacebo_available={available} libplacebo_usable=false vulkan_initialised={vulkan}", session.id);
        }
        return Err(reason);
    }
    if decoder.mpv.get_property("seekable", Kind::Flag) != Some(true.into()) {
        return Err("not-seekable");
    }
    let profile = video_profile(&decoder.mpv).or(session.source.dv_profile);
    if profile.is_some_and(|p| !matches!(p, 5 | 7..=9)) {
        return Err("dolby-vision-profile-unsupported");
    }
    let mut changed = false;
    let detected = colour_plan(&decoder.mpv, &session.source);
    if detected != PreviewColour::Unknown && detected != plan {
        // Reconcile metadata with the selected decoded track before caching any frame.
        plan = PreviewColour::Unknown;
    }
    if plan == PreviewColour::Unknown {
        plan = colour_plan(&decoder.mpv, &PreviewSource::default());
        if plan == PreviewColour::Unknown && profile.is_some_and(|p| matches!(p, 7..=9)) {
            // Unknown compatible base transfer: use the slower, detect-then-swap fallback.
            decoder
                .mpv
                .set_property("vf", "format=dolbyvision=no,scale=320:-2")
                .map_err(|_| "dv-base-filter")?;
            while decoder.mpv.wait_event(0.0, |_| false).is_some() {}
            let at = number(&decoder.mpv, "time-pos").ok_or("no-position")?;
            decoder
                .mpv
                .command(&["seek", &at.to_string(), "absolute", "keyframes"])
                .map_err(|_| "dv-base-seek")?;
            wait_frame_for(
                &decoder.mpv,
                shared,
                generation,
                Duration::from_secs(8),
                WaitPolicy::new(WorkKind::Demand, true),
                None,
            )?;
            plan = match decoder
                .mpv
                .get_property("video-out-params/gamma", Kind::String)
                .and_then(|v| v.as_str().map(str::to_owned))
                .as_deref()
            {
                Some("pq") => PreviewColour::DvPq,
                Some("hlg") => PreviewColour::DvHlg,
                Some("bt.1886" | "srgb" | "gamma2.2" | "gamma2.4") => PreviewColour::DvSdr,
                _ => return Err("dv-base-colour-unavailable"),
            };
        }
        if plan == PreviewColour::Unknown {
            let matrix = decoder
                .mpv
                .get_property("video-dec-params/colormatrix", Kind::String);
            if matrix.as_ref().and_then(|v| v.as_str()) == Some("dolbyvision") {
                return Err("dolby-vision-needs-conversion");
            }
            plan = PreviewColour::Sdr;
        }
        if plan == PreviewColour::Dv && !p5 {
            p5 = true;
            let _ = decoder.mpv.request_log_messages("v");
            log::info!(target: "seek_preview", "session={} p5_filter=attempting filter=libplacebo detection=after-load vulkan_initialised=unknown dv_metadata_delivery=unverified", session.id);
        }
        decoder
            .mpv
            .set_property("vf", &preview_filter(plan, variant))
            .map_err(|_| "colour-filter")?;
        changed = true;
    }
    if matches!(
        plan,
        PreviewColour::Pq
            | PreviewColour::Hlg
            | PreviewColour::DvPq
            | PreviewColour::DvHlg
            | PreviewColour::Dv
    ) || p5
    {
        let prepared = Instant::now();
        let limit = if p5 {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(8)
        };
        while !sdr_output(&decoder.mpv)
            || decoder.mpv.get_property("seeking", Kind::Flag) == Some(true.into())
        {
            if !active(shared, generation) {
                return Err("cancelled");
            }
            if prepared.elapsed() >= limit {
                if p5 {
                    log::info!(target: "seek_preview", "session={} p5_filter=failed:output-not-ready libplacebo_usable=false", session.id);
                }
                return Err("colour-filter-not-ready");
            }
            if let Some(
                Event::EndFile {
                    reason: "error", ..
                }
                | Event::Shutdown,
            ) = decoder.mpv.wait_event(0.05, |_| false)
            {
                if p5 {
                    log::info!(target: "seek_preview", "session={} p5_filter=failed:decode-error libplacebo_usable=false vulkan_initialised=unknown", session.id);
                }
                return Err("decode-error");
            }
        }
        log::info!(target: "seek_preview", "session={} hdr_filter_ready_ms={} filter_changed_after_load={changed}", session.id, prepared.elapsed().as_millis());
    }
    if plan == PreviewColour::Dv || p5 {
        let width = number(&decoder.mpv, "video-out-params/w").unwrap_or(0.0);
        let height = number(&decoder.mpv, "video-out-params/h").unwrap_or(0.0);
        if !(1.0..=324.0).contains(&width)
            || !(1.0..=4096.0).contains(&height)
            || !sdr_output(&decoder.mpv)
        {
            log::info!(target: "seek_preview", "session={} p5_filter=failed:invalid-output vf_width={width} vf_height={height}", session.id);
            return Err("p5-invalid-output");
        }
        log::info!(target: "seek_preview", "session={} p5_filter=ok libplacebo_available=true libplacebo_usable=true vulkan_initialised=true has_video_out=true vf_width={width} vf_height={height} dv_metadata_delivery=unverified colour_visual=unverified", session.id);
    }
    log::info!(target: "seek_preview", "session={} opened_ms={} reopened={reopened} hdr={} threads={threads} test_variant={variant} colour_path={} dv_profile={profile:?} filter_changed_after_load={changed}", session.id, start.elapsed().as_millis(), plan != PreviewColour::Sdr, colour_name(plan));
    Ok(decoder)
}

struct Capture {
    target: u32,
    kind: WorkKind,
    step: f64,
    reopened: bool,
    preempt_before_ms: Option<u64>,
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
        preempt_before_ms,
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
            WaitPolicy {
                kind,
                require_seek: true,
                target: Some(target),
                step,
                preempt_before_ms,
            },
            Some(main),
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
    let screenshot_at = Instant::now();
    let raw = decoder.mpv.screenshot().map_err(|_| "screenshot-raw")?;
    let screenshot_ms = screenshot_at.elapsed().as_millis();
    let jpeg_at = Instant::now();
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 70)
        .encode(
            &raw.rgb,
            raw.width,
            raw.height,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|_| "jpeg-encode")?;
    let jpeg_encode_ms = jpeg_at.elapsed().as_micros() as f64 / 1000.0;
    if bytes.len() > 512 * 1024 || !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("invalid-image");
    }
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
    log::info!(target: "seek_preview", "session={} bucket={} target_s={wanted_s:.3} sampled_s={position:.3} covers_from_s={:.3} covers_until_s={:.3} offset_s={:.3} demand={} seek_ms={seek_ms} capture_ms={} screenshot_ms={screenshot_ms} screenshot_path=memory jpeg_encode_ms={jpeg_encode_ms:.3} image_width={} image_height={} base64_ms={base64_ms:.3} image_bytes={} playhead_distance_s={:?} main_buffered={main_buffered:?} reopened={reopened} vf_width={width:?} vf_height={height:?} aspect_ratio={aspect:.5} output_transfer={transfer} open_frame={open_frame} demux_queue_total_bytes={queue_total:?} demux_queue_forward_bytes={queue_forward:?} raw_input_bytes_per_second={raw_rate:?}", session.id, target, wanted_s.min(position), wanted_s.max(position), position - wanted_s, demand, screenshot_at.elapsed().as_millis(), raw.width, raw.height, bytes.len(), playhead.map(|v| wanted_s-v));
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

fn bucket_count(duration: f64, step: f64) -> u32 {
    (duration.clamp(0.0, 86400.0) / step).ceil() as u32
}

fn covered_buckets(cache: &VecDeque<Image>, duration: f64, step: f64) -> BTreeSet<u32> {
    // Cover the entire bucket, not only its centre. An opening frame can cover the
    // centre while leaving an edge uncached, and two adjacent images can fill it.
    let mut ranges: Vec<_> = cache
        .iter()
        .map(|image| {
            (
                image.covers_from - step / 2.0,
                image.covers_until + step / 2.0,
            )
        })
        .collect();
    ranges.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let mut have = BTreeSet::new();
    let mut range = 0;
    for b in 0..bucket_count(duration, step) {
        let start = f64::from(b) * step;
        let end = ((f64::from(b) + 1.0) * step).min(duration);
        while range < merged.len() && merged[range].1 < start {
            range += 1;
        }
        if merged
            .get(range)
            .is_some_and(|r| r.0 <= start && r.1 >= end)
        {
            have.insert(b);
        }
    }
    have
}

fn next_missing(have: &BTreeSet<u32>, playhead: u32, last: u32, turn: usize) -> Option<u32> {
    let playhead = playhead.min(last);
    let ahead = (playhead..=last).find(|b| !have.contains(b));
    let behind = (0..playhead).rev().find(|b| !have.contains(b));
    // Two ahead, then one behind. Recompute from actual cached coverage so a moving
    // playhead or cache eviction cannot leave permanently skipped positions.
    if turn % 3 == 2 {
        behind.or(ahead)
    } else {
        ahead.or(behind)
    }
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
        || cache.iter().map(|i| i.data.len()).sum::<usize>() > MAX_CACHE_BYTES
    {
        cache.pop_front();
    }
}

fn room_for_image(cache: &VecDeque<Image>, image: &Image) -> bool {
    let retained: Vec<_> = cache
        .iter()
        .filter(|old| old.bucket != image.bucket)
        .collect();
    retained.len() < MAX_IMAGES
        && retained.iter().map(|old| old.data.len()).sum::<usize>() + image.data.len()
            <= MAX_CACHE_BYTES
}

fn log_coverage(session: &str, cache: &VecDeque<Image>, duration: f64, position: f64, step: f64) {
    let count = bucket_count(duration, step);
    if count == 0 {
        return;
    }
    let have = covered_buckets(cache, duration, step);
    let from = bucket((position - 300.0).max(0.0), step).unwrap_or(0);
    let through = bucket((position + 300.0).min((duration - 0.05).max(0.0)), step)
        .unwrap_or(0)
        .min(count - 1);
    let holes = (from..=through).filter(|b| !have.contains(b)).count();
    log::info!(target: "seek_preview", "session={session} coverage_pct={:.1} covered_buckets={} total_buckets={count} holes_within_5min_of_playhead={holes} playhead_s={position:.3} cached_images={} cache_encoded_bytes={}", 100.0 * have.len() as f64 / f64::from(count), have.len(), cache.len(), cache.iter().map(|i| i.data.len()).sum::<usize>());
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
        let mut recent_seeks = VecDeque::new();
        let mut neighbor_done = None;
        let mut decoder = None;
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
        let mut duration_s = 0.0;
        let mut playhead_s = 0.0;
        let mut fresh_open = false;
        let mut sent_aspect = None;
        let mut last_speculative: Option<(Instant, u64)> = None;
        let mut background_turn = 0;
        let mut cache_full = false;
        let mut coverage_at = Instant::now();
        let mut first_wait_reason = "";
        let mut demand_preemptions = 0;
        log::info!(target: "seek_preview", "session={} start cached_debrid=true max_background_work_ms={MAX_BACKGROUND_WORK_MS} max_completed_captures={MAX_CAPTURES} max_demand_work_ms={MAX_DEMAND_WORK_MS} max_images={MAX_IMAGES} cache_encoded_limit_bytes=8388608 min_background_buffer_s=10 background_duty_limit=0.25..0.5 background_input_budget=none network_bytes=unavailable test_variant={} colour_path={} idle_close_s={IDLE_CLOSE_S}", session.id, test_variant(), colour_name(session.source.colour));
        while active(shared, generation) {
            let calm = healthy(main, &session.url);
            let hover_ok = hover_allowed(main, &session.url);
            if !configured
                && source_active(main, &session.url)
                && let Some(duration) = number(main, "duration").filter(|s| *s > 0.0)
            {
                step = step_for(duration);
                duration_s = duration;
                configured = true;
                status = "";
                log::info!(target: "seek_preview", "session={} step_ms={} duration_s={duration:.3}", session.id, (step * 1000.0) as u32);
            }
            if source_active(main, &session.url) {
                playhead_s = number(main, "time-pos").unwrap_or(playhead_s);
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
            if decoder.is_some() && last_work.elapsed() > Duration::from_secs(IDLE_CLOSE_S) {
                decoder = None;
                log::info!(target: "seek_preview", "session={} decoder_closed=idle", session.id);
            }
            let buffering = main.get_property("paused-for-cache", Kind::Flag) == Some(true.into());
            if buffering && !was_buffering {
                stats.buffering_intervals += 1;
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
            let playing = calm && main.get_property("pause", Kind::Flag) == Some(false.into());
            let cache_state = main.get_property("demuxer-cache-state", Kind::Json);
            let reader_idle = cache_state
                .as_ref()
                .and_then(|v| v.get("idle"))
                .and_then(|v| v.as_bool())
                == Some(true)
                && cache_state
                    .as_ref()
                    .and_then(|v| v.get("underrun"))
                    .and_then(|v| v.as_bool())
                    != Some(true);
            let duty = background_duty(playing, buffered, current.pointer_on_timeline, reader_idle);
            let background_ok = duty > 0.0;
            let bg_state = if stats.background_work_ms >= MAX_BACKGROUND_WORK_MS {
                "work-budget"
            } else if cache_full {
                "cache-size-limit"
            } else if current.pointer_on_timeline {
                "pointer"
            } else if !playing {
                "playback-busy"
            } else if !background_ok {
                "buffer-low-or-unknown"
            } else {
                "ready"
            };
            if bg_state != background_status {
                background_status = bg_state;
                log::info!(target: "seek_preview", "session={} background_state={bg_state} main_buffer_ahead_s={buffered:?} main_reader_idle={reader_idle} background_duty={duty} background_work_ms={} network_bytes=unavailable", session.id, stats.background_work_ms);
            }
            if configured && coverage_at.elapsed() >= Duration::from_secs(30) {
                log_coverage(&session.id, &cache, duration_s, playhead_s, step);
                coverage_at = Instant::now();
            }
            if let Some(first_hover_at) = current.first_hover_at
                && !current.first_display_reported
            {
                let reason = if failed {
                    "unavailable"
                } else if !hover_ok {
                    "main-first-frame-or-seek"
                } else if decoder.is_none() {
                    "decoder-open"
                } else if current.wanted.is_some() {
                    "decoder-seek"
                } else {
                    "pointer-settle-or-cached"
                };
                if reason != first_wait_reason {
                    first_wait_reason = reason;
                    log::info!(target: "seek_preview", "session={} first_hover_wait_reason={reason} since_first_hover_ms={} main_buffer_ahead_s={buffered:?} main_buffering={buffering}", session.id, first_hover_at.elapsed().as_millis());
                }
            }
            if !hover_ok || failed || !configured {
                std::thread::park_timeout(Duration::from_millis(100));
                continue;
            }
            let wanted_seconds = current.wanted;
            let wanted = wanted_seconds.and_then(|seconds| bucket(seconds, step));
            if let Some(target) = wanted
                && let Some(image) = nearby(&cache, wanted_seconds.unwrap(), step / 2.0)
            {
                stats.hits += 1;
                deliver(emit, &session, image, true, 0);
                finish_request(shared, generation, target, step);
                continue;
            }
            let opening_on_demand =
                wanted.is_some() || (stats.opens == 0 && current.pointer_on_timeline);
            let preload = if stats.opens == 0 {
                playing && buffered.is_some_and(|s| s >= 3.0)
            } else if background_ok
                && !cache_full
                && stats.background_work_ms < MAX_BACKGROUND_WORK_MS
            {
                let duration = number(main, "duration").unwrap_or(0.0);
                let count = bucket_count(duration, step);
                count > 0
                    && next_missing(
                        &covered_buckets(&cache, duration, step),
                        bucket(number(main, "time-pos").unwrap_or(0.0), step).unwrap_or(0),
                        count - 1,
                        background_turn,
                    )
                    .is_some()
            } else {
                false
            };
            if decoder.is_none() && (opening_on_demand || preload) {
                if Instant::now() < retry_at {
                    std::thread::park_timeout(Duration::from_millis(100));
                    continue;
                }
                let opening_at = Instant::now();
                match open(library, main, &session, shared, generation, stats.opens > 0) {
                    Ok(d) => {
                        if !opening_on_demand {
                            let elapsed = opening_at.elapsed().as_millis() as u64;
                            stats.background_work_ms += elapsed;
                            last_speculative = Some((Instant::now(), elapsed));
                        }
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
                                preempt_before_ms: None,
                            },
                        ) {
                            Ok(image) if active(shared, generation) => {
                                stats.generated += 1;
                                stats.work_ms += opened_at.elapsed().as_millis() as u64;
                                // Reopening must not replace a complete slot with a
                                // smaller opening-frame interval in either cache.
                                if !cache.iter().any(|old| {
                                    old.bucket == image.bucket
                                        && old.covers_from <= image.covers_from
                                        && old.covers_until >= image.covers_until
                                }) {
                                    deliver(
                                        emit,
                                        &session,
                                        &image,
                                        false,
                                        opened_at.elapsed().as_millis() as u64,
                                    );
                                    keep_image(&mut cache, image);
                                }
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
                        stats.failed_work_ms += opening_at.elapsed().as_millis() as u64;
                        consecutive_errors += 1;
                        retry_at = Instant::now() + Duration::from_secs(1);
                        // Unsupported colour/filter paths fail fast; a transient remote
                        // open gets two retries, bounded by the existing failure ceiling.
                        failed = !matches!(reason, "timeout" | "decode-error" | "open-command")
                            || consecutive_errors >= 3
                            || stats.failed_work_ms >= MAX_FAILED_WORK_MS;
                    }
                }
                continue;
            }
            let mut kind = WorkKind::Demand;
            let target = wanted.or_else(|| {
                if stats.background_work_ms >= MAX_BACKGROUND_WORK_MS
                    || cache_full
                    || Instant::now() < background_at
                    || last_speculative.is_some_and(|(at, elapsed)| {
                        at.elapsed()
                            < speculative_rest(
                                elapsed,
                                if current.pointer_on_timeline {
                                    background_duty(playing, buffered, false, reader_idle)
                                } else {
                                    duty
                                },
                            )
                    })
                {
                    return None;
                }
                if current.pointer_on_timeline {
                    // One neighbor only, after a fast hover finishes and rests for 350 ms.
                    if background_duty(playing, buffered, false, reader_idle) == 0.0
                        || current.hover_pending
                        || median_seek(&recent_seeks).is_none_or(|ms| ms >= 1000)
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
                let count = bucket_count(duration, step);
                if count == 0 {
                    return None;
                }
                let have = covered_buckets(&cache, duration, step);
                let playhead = bucket(position, step)?.min(count - 1);
                let next = next_missing(&have, playhead, count - 1, background_turn)?;
                kind = WorkKind::Background;
                Some(next)
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
            if decoder.is_none() {
                std::thread::park_timeout(Duration::from_millis(100));
                continue;
            }
            if stats.generated >= MAX_CAPTURES
                || stats.demand_work_ms >= MAX_DEMAND_WORK_MS
                || stats.failed_work_ms >= MAX_FAILED_WORK_MS
            {
                failed = true;
                decoder = None;
                log::info!(target: "seek_preview", "session={} budget_exhausted=true generated={} work_ms={} failed_work_ms={}", session.id, stats.generated, stats.work_ms, stats.failed_work_ms);
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
                    preempt_before_ms: if demand && demand_preemptions == 0 {
                        median_seek(&recent_seeks).map(|ms| (ms / 2).clamp(50, 500))
                    } else {
                        None
                    },
                },
            );
            let elapsed = started.elapsed().as_millis() as u64;
            if !demand {
                stats.background_work_ms += elapsed;
                last_speculative = Some((Instant::now(), elapsed));
            }
            if let Err(reason) = result.as_ref() {
                // Expected speculative interruptions already spend the background budget.
                // They must not disable later demand work through the failure guard.
                if !matches!(
                    *reason,
                    "cancelled" | "superseded-background" | "superseded-demand" | "playback-busy"
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
                    if demand {
                        stats.demand_work_ms += elapsed;
                    }
                    if kind == WorkKind::Background {
                        background_turn += 1;
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
                    // Speculation must not endlessly evict and recapture the same slots
                    // after the encoded-size limit. Demand may still replace old images.
                    if !demand && !room_for_image(&cache, &image) {
                        cache_full = true;
                    } else {
                        deliver(emit, &session, &image, false, elapsed);
                        keep_image(&mut cache, image);
                    }
                    backoff = Duration::from_millis((elapsed / 2).clamp(100, 1500));
                    fresh_open = false;
                }
                Err("cancelled") => break,
                Err("superseded-demand") => {
                    stats.preempted += 1;
                    demand_preemptions += 1;
                }
                Err("superseded-background" | "playback-busy") => {
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
                demand_preemptions = 0;
                finish_request(shared, generation, target, step);
            }
            background_at = Instant::now() + backoff;
        }
        drop(decoder);
        log_coverage(&session.id, &cache, duration_s, playhead_s, step);
        log::info!(target: "seek_preview", "session={} summary generated={} background_candidates={} background_work_ms={} neighbors={} preempted={} native_hits={} errors={} completed_after_move={} opens={} readiness_wait_intervals={} main_buffering_intervals={} attempts={} work_ms={} failed_work_ms={} demand_work_ms={} network_bytes=unavailable", session.id, stats.generated, stats.background, stats.background_work_ms, stats.neighbors, stats.preempted, stats.hits, stats.errors, stats.completed_after_move, stats.opens, stats.waiting, stats.buffering_intervals, stats.attempts, stats.work_ms, stats.failed_work_ms, stats.demand_work_ms);
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
    use std::path::PathBuf;
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
    fn duration_based_grid_and_sample_time_cache() {
        assert_eq!(step_for(1799.0), 5.0);
        assert_eq!(step_for(1800.0), 5.0);
        assert_eq!(step_for(7200.0), 10.0);
        assert_eq!(step_for(15000.0), 25.0);
        assert_eq!(step_for(86400.0), 30.0);
        assert_eq!(step_for(0.0), 10.0);
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
            source: PreviewSource::default(),
            wanted: None,
            pointer_on_timeline: false,
            last_hover: None,
            hover_direction: 1,
            hover_settled_at: Instant::now(),
            hover_pending: true,
            first_hover_at: None,
            first_display_reported: false,
        };
        assert!(!should_yield(&session, WorkKind::Background));
        session.pointer_on_timeline = true;
        session.hover_pending = false;
        assert!(should_yield(&session, WorkKind::Background));
        assert!(!should_yield(&session, WorkKind::Neighbor));
        session.wanted = Some(10.0);
        assert!(should_yield(&session, WorkKind::Neighbor));
        assert!(!should_yield(&session, WorkKind::Demand));
        assert_eq!(background_duty(true, None, false, false), 0.0);
        assert_eq!(background_duty(true, Some(9.9), false, false), 0.0);
        assert_eq!(background_duty(false, Some(30.0), false, false), 0.0);
        assert_eq!(background_duty(true, Some(30.0), true, true), 0.0);
        assert_eq!(background_duty(true, Some(10.0), false, false), 0.25);
        assert_eq!(
            median_seek(&VecDeque::from([200, 1200, 250, 300, 275])),
            Some(275)
        );
        assert_eq!(median_seek(&VecDeque::from([200, 250])), None);
    }

    #[test]
    fn colour_filters_and_background_duty_are_bounded() {
        assert!(preview_filter(PreviewColour::Dv, "default").contains("apply_dolbyvision=true"));
        assert!(
            preview_filter(PreviewColour::DvPq, "default")
                .starts_with("format=dolbyvision=no,lavfi=[scale=")
        );
        assert!(preview_filter(PreviewColour::Pq, "tonemap-first").starts_with("lavfi=[zscale="));
        assert_eq!(background_duty(true, Some(4.0), false, true), 0.0);
        assert_eq!(background_duty(true, Some(29.9), false, true), 0.25);
        assert_eq!(background_duty(true, Some(30.0), false, true), 0.5);
        assert_eq!(background_duty(true, Some(100.0), false, false), 0.25);
        assert_eq!(background_duty(true, Some(f64::NAN), false, true), 0.0);
        assert_eq!(speculative_rest(231, 0.5).as_millis(), 231);
        assert_eq!(speculative_rest(231, 0.25).as_millis(), 693);
    }

    #[test]
    fn cache_coverage_has_no_edge_gaps_and_recovers_after_eviction() {
        let image = |b, from, until| Image {
            bucket: b,
            position: from,
            covers_from: from,
            covers_until: until,
            data: String::new(),
            aspect_ratio: 16.0 / 9.0,
            seek_ms: 0,
        };
        let mut cache = VecDeque::from([image(0, 5.2, 5.2)]);
        // An opening frame covers the centre, but not the first 0.2 seconds.
        assert!(nearby(&cache, 5.0, 5.0).is_some());
        assert!(!covered_buckets(&cache, 23.0, 10.0).contains(&0));
        cache = VecDeque::from([
            image(0, 4.0, 5.0),
            image(1, 14.0, 15.0),
            image(2, 22.0, 22.95),
        ]);
        assert_eq!(
            covered_buckets(&cache, 23.0, 10.0),
            BTreeSet::from([0, 1, 2])
        );
        for tenth in 0..=230 {
            assert!(nearby(&cache, f64::from(tenth) / 10.0, 5.0).is_some());
        }
        cache.remove(1);
        let have = covered_buckets(&cache, 23.0, 10.0);
        assert_eq!(next_missing(&have, 2, 2, 0), Some(1));
    }

    #[test]
    fn missing_scheduler_fills_all_slots_even_as_playhead_moves() {
        let mut have = BTreeSet::new();
        for turn in 0..143 {
            let moving_playhead = (turn / 5).min(142) as u32;
            let b = next_missing(&have, moving_playhead, 142, turn).unwrap();
            assert!(have.insert(b), "scheduler repeated an existing slot");
        }
        assert_eq!(have.len(), 143);
        assert_eq!(next_missing(&have, 100, 142, 143), None);
        have.remove(&60);
        assert_eq!(next_missing(&have, 100, 142, 144), Some(60));
    }

    #[test]
    #[ignore = "requires libmpv and a real Dolby Vision profile 5 sample"]
    fn dv5_conversion_or_bounded_unavailable() {
        log::set_logger(&TEST_LOG).unwrap();
        log::set_max_level(log::LevelFilter::Info);
        let library = PathBuf::from(std::env::var("PREVIEW_TEST_LIBMPV").unwrap());
        let url = std::env::var("PREVIEW_TEST_P5_FILE").unwrap();
        let source = PreviewSource {
            colour: PreviewColour::Dv,
            dv_profile: Some(5),
            bitrate: Some(15_184_992.0),
        };
        let main = Mpv::new(
            &library,
            &[
                ("config", "no"),
                ("load-scripts", "no"),
                ("vo", "null"),
                ("audio", "no"),
                ("pause", "yes"),
                ("idle", "yes"),
            ],
        )
        .unwrap();
        let session = Session {
            id: "real-p5".into(),
            url: url.clone(),
            source,
            wanted: None,
            pointer_on_timeline: false,
            last_hover: None,
            hover_direction: 1,
            hover_settled_at: Instant::now(),
            hover_pending: true,
            first_hover_at: None,
            first_display_reported: false,
        };
        let shared: Shared = Arc::default();
        {
            let mut state = shared.0.lock().unwrap();
            state.generation = 1;
            state.session = Some(session.clone());
        }
        main.command(&["loadfile", &url]).unwrap();
        wait_frame(&main, &shared, 1, Duration::from_secs(15)).unwrap();
        while main.wait_event(0.0, |_| false).is_some() {}
        main.command(&["seek", "8", "absolute", "keyframes"])
            .unwrap();
        wait_frame_for(
            &main,
            &shared,
            1,
            Duration::from_secs(15),
            WaitPolicy::new(WorkKind::Demand, true),
            None,
        )
        .unwrap();
        let started = Instant::now();
        match open(&library, &main, &session, &shared, 1, false) {
            Ok(decoder) => {
                let image = capture(
                    &decoder,
                    &main,
                    &session,
                    &shared,
                    1,
                    Capture {
                        target: 1,
                        kind: WorkKind::OpenFrame,
                        step: 5.0,
                        reopened: false,
                        preempt_before_ms: None,
                    },
                )
                .unwrap();
                assert!(sdr_output(&decoder.mpv));
                if let Ok(dir) = std::env::var("PREVIEW_TEST_ARCHIVE") {
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(
                        Path::new(&dir).join("real-p5.jpg"),
                        STANDARD
                            .decode(image.data.strip_prefix("data:image/jpeg;base64,").unwrap())
                            .unwrap(),
                    )
                    .unwrap();
                }
                eprintln!("REAL_P5_RESULT=converted visual_colour=unverified");
            }
            Err(reason) => {
                assert!(
                    matches!(
                        reason,
                        "decoder-init"
                            | "decode-error"
                            | "timeout"
                            | "colour-filter"
                            | "colour-filter-not-ready"
                            | "p5-invalid-output"
                            | "vulkan-unavailable"
                            | "libplacebo-missing"
                            | "libplacebo-options-unavailable"
                            | "colour-filter-failed"
                    ),
                    "unexpected P5 failure: {reason}"
                );
                assert!(
                    started.elapsed() < Duration::from_secs(8),
                    "P5 conversion did not fail within its bounded startup wait"
                );
                eprintln!("REAL_P5_RESULT=unavailable reason={reason}");
            }
        }
        assert_eq!(main.get_property("pause", Kind::Flag), Some(true.into()));
        assert!(
            number(&main, "time-pos").is_some_and(|v| (6.0..=9.0).contains(&v)),
            "P5 preview changed main playback"
        );
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
                source: PreviewSource::default(),
                wanted: None,
                pointer_on_timeline: false,
                last_hover: None,
                hover_direction: 1,
                hover_settled_at: Instant::now(),
                hover_pending: true,
                first_hover_at: None,
                first_display_reported: false,
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
        let automatic = std::env::var("PREVIEW_TEST_AUTO_OPEN").as_deref() == Ok("yes");
        if automatic {
            main.set_property("pause", "no").unwrap();
        }
        worker.start("fixture-one".into(), url.clone());
        if !automatic {
            worker.request("fixture-one", Some(0.0));
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let first = loop {
            assert!(Instant::now() < deadline, "no first thumbnail");
            if let Outbound::PreviewFrame { image, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break image;
            }
        };
        // Exercise the production scheduler only while playback is running.
        main.set_property("pause", "no").unwrap();
        loop {
            assert!(
                Instant::now() < deadline,
                "background prefetch did not run with healthy buffering"
            );
            if let Outbound::PreviewFrame { bucket: 2, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break;
            }
        }
        main.set_property("pause", "yes").unwrap();
        // Use a fresh session to keep the cold-demand regression independent of
        // the background pass, which can now fill this whole short fixture.
        worker.stop("fixture-one");
        worker.start("fixture-demand".into(), url.clone());
        worker.hover("fixture-demand", true);
        *CHANGE_ON_SEEK.lock().unwrap() = Some((worker.shared.clone(), Some(22.5)));
        worker.request("fixture-demand", Some(15.1));
        let (later, position) = loop {
            assert!(Instant::now() < deadline, "no requested thumbnail");
            if let Outbound::PreviewFrame {
                image,
                bucket: 3,
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
            .unwrap_or(16.0);
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
        worker.request("fixture-demand", Some(0.0));
        worker.request("fixture-demand", Some(10.0));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "latest requested position lost");
            if let Outbound::PreviewFrame {
                covers_from,
                covers_until,
                ..
            } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                // A long-GOP frame captured for another slot can already cover
                // this request. Native cache hits preserve the producer bucket.
                if 10.0 >= covers_from - 2.5 && 10.0 <= covers_until + 2.5 {
                    break;
                }
            }
        }
        main.command(&["seek", "6", "absolute", "keyframes"])
            .unwrap();
        worker.request("fixture-demand", Some(16.0));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "hover after a main-player seek stalled"
            );
            if let Outbound::PreviewFrame {
                covers_from,
                covers_until,
                ..
            } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                // Ignore older queued frames. A neighbouring keyframe can serve
                // the hover, so check coverage rather than its producer slot id.
                if 16.0 >= covers_from - 2.5 && 16.0 <= covers_until + 2.5 {
                    break;
                }
            }
        }
        assert_eq!(
            OPEN_COUNT.load(Ordering::Relaxed),
            2,
            "main-player seek reopened the decoder"
        );
        worker.stop("fixture-demand");
        while receive.try_recv().is_ok() {}
        worker.request("fixture-demand", Some(10.0));
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
                preempt_before_ms: None,
            },
        )
        .expect("clearing the hover discarded a paid-for seek");
        assert_eq!(completed.covers_until, 12.5);
        let cache = VecDeque::from([completed]);
        assert!(nearby(&cache, 10.1, 2.5).is_some());
        assert!(nearby(&cache, 15.1, 2.5).is_none());
        shared
            .0
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .pointer_on_timeline = true;
        *CHANGE_ON_SEEK.lock().unwrap() = Some((shared.clone(), Some(10.1)));
        let early = capture(
            &direct,
            &main,
            &session,
            &shared,
            1,
            Capture {
                target: 4,
                kind: WorkKind::Demand,
                step: 5.0,
                reopened: false,
                preempt_before_ms: Some(500),
            },
        );
        assert!(
            early.is_ok() || matches!(early, Err("superseded-demand")),
            "unexpected demand cancellation"
        );
        if std::env::var("PREVIEW_TEST_REQUIRE_PREEMPT").as_deref() == Ok("yes") {
            assert!(
                matches!(early, Err("superseded-demand")),
                "slow remote demand did not yield to the newer target"
            );
        }
        capture(
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
                preempt_before_ms: None,
            },
        )
        .expect("demand failed to recover after early cancellation");
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
                preempt_before_ms: None,
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
                preempt_before_ms: None,
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
                WaitPolicy::new(WorkKind::Demand, true),
                None,
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
                WaitPolicy::new(WorkKind::Demand, true),
                None,
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
                    preempt_before_ms: None,
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
