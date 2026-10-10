//! Session-scoped seek images. A second, small decoder never seeks the playing decoder.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::bridge::{Outbound, PreviewColour, PreviewMode, PreviewSource, PreviewSourceKind};
use crate::mpv::{Event, Kind, Mpv};
use crate::player::Emit;
use crate::preview_policy::{self, Controller, Costs, Health, MAX_GRID_SLOTS};

// Bound remote extraction work, not just the memory used by completed images.
const MAX_IMAGES: usize = MAX_GRID_SLOTS as usize;
const MAX_CACHE_BYTES: usize = 24 * 1024 * 1024;
// Absorb floating-point arithmetic noise at shared slot boundaries (one microsecond).
const COVERAGE_EPSILON_S: f64 = 0.000_001;
const MAX_BACKGROUND_WORK_MS: u64 = 7_200_000;
// Repeated failure work lengthens recovery cooldown; it never disables demand.
const FAILED_WORK_COOLDOWN_MS: u64 = 90_000;
const IDLE_CLOSE_S: u64 = 600;

#[derive(Clone)]
struct Session {
    id: String,
    url: String,
    source: PreviewSource,
    wanted: Option<f64>,
    pointer_on_timeline: bool,
    first_hover_at: Option<Instant>,
    first_display_reported: bool,
    cached_displays: Arc<AtomicU64>,
}

#[derive(Default)]
struct State {
    generation: u64,
    session: Option<Session>,
    shutdown: bool,
    last_main_seek: Option<Instant>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct Previews {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl Previews {
    pub fn new(library: &Path, main: Arc<Mpv>, emit: Emit) -> Result<Self, String> {
        Self::with_data_dir(library, main, emit, None)
    }

    pub fn with_data_dir(
        library: &Path,
        main: Arc<Mpv>,
        emit: Emit,
        data_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let data_dir = data_dir.map(Path::to_path_buf);
        let shared: Shared = Arc::default();
        let thread = std::thread::Builder::new()
            .name("seek-previews".into())
            .spawn({
                let shared = shared.clone();
                let library = library.to_path_buf();
                move || run(&library, &main, &emit, &shared, data_dir.as_deref())
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

    pub fn start_with_source(&self, id: String, url: String, mut source: PreviewSource) {
        // The native worker enforces this too, rather than trusting the page's mode.
        source.mode = preview_policy::enforced_mode(
            source.kind,
            source.mode,
            preview_policy::usenet_background_enabled(),
            preview_policy::http_background_enabled(),
        );
        let mut state = self.shared.0.lock().unwrap();
        state.generation += 1;
        state.last_main_seek = None;
        if source.mode == PreviewMode::Off {
            state.session = None;
            self.shared.1.notify_one();
            return;
        }
        state.session = Some(Session {
            id,
            url,
            source,
            wanted: None,
            pointer_on_timeline: false,
            first_hover_at: None,
            first_display_reported: false,
            cached_displays: Arc::default(),
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

    pub fn main_seek(&self) {
        self.shared.0.lock().unwrap().last_main_seek = Some(Instant::now());
        self.shared.1.notify_one();
    }

    pub fn request(&self, id: &str, seconds: Option<f64>) {
        let mut state = self.shared.0.lock().unwrap();
        if let Some(s) = state.session.as_mut().filter(|s| s.id == id) {
            s.wanted = seconds.filter(|v| valid_position(*v));
            if !s.first_display_reported {
                if s.wanted.is_some() {
                    s.first_hover_at.get_or_insert_with(Instant::now);
                } else {
                    s.first_hover_at = None;
                }
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
                if !session.first_display_reported {
                    session.first_hover_at = None;
                }
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
            state.session.as_mut().filter(|s| s.id == id).and_then(|s| {
                if event == "display-cached" {
                    s.cached_displays.fetch_add(1, Ordering::Relaxed);
                }
                if s.first_display_reported {
                    return None;
                }
                let at = s.first_hover_at?;
                s.first_display_reported = true;
                Some(at.elapsed().as_millis())
            })
        };
        if let Some(elapsed) = elapsed {
            log::info!(target: "seek_preview", "session={id} first_image_since_first_hover_ms={elapsed} hover_timer=settled-demand display_kind={event}");
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
}

fn should_yield(session: &Session, kind: WorkKind) -> bool {
    match kind {
        WorkKind::Background => session.pointer_on_timeline || session.wanted.is_some(),
        // Keep demand captures live on long-GOP sources, even during rapid movement.
        WorkKind::OpenFrame | WorkKind::Demand => false,
    }
}

// Use the same cancellation gates for initial decode, colour preparation and seeks.
fn background_wait_gate(
    shared: &Shared,
    main: Option<&Mpv>,
    opening: bool,
) -> Result<(), &'static str> {
    let (url, conservative) = {
        let state = shared.0.lock().unwrap();
        let Some(session) = &state.session else {
            return Err("cancelled");
        };
        if should_yield(session, WorkKind::Background) {
            return Err("superseded-background");
        }
        (
            session.url.clone(),
            preview_policy::conservative_source(session.source.kind),
        )
    };
    if main.is_some_and(|m| {
        let cache = m.get_property("demuxer-cache-state", Kind::Json);
        !source_current(m, &url)
            || m.get_property("paused-for-cache", Kind::Flag) == Some(true.into())
            || m.get_property("seeking", Kind::Flag) == Some(true.into())
            || cache
                .as_ref()
                .and_then(|v| v.get("underrun"))
                .and_then(|v| v.as_bool())
                == Some(true)
            || number(m, "demuxer-cache-duration").is_none_or(|s| {
                s < if conservative {
                    preview_policy::CONSERVATIVE_ABORT_BUFFER_S
                } else if opening {
                    3.0
                } else {
                    10.0
                }
            })
            || conservative
                && cache
                    .as_ref()
                    .and_then(|v| v.get("idle"))
                    .and_then(|v| v.as_bool())
                    != Some(true)
    }) {
        Err("playback-busy")
    } else {
        Ok(())
    }
}

fn speculative_rest(elapsed_ms: u64, duty: f64) -> Duration {
    if duty <= 0.0 {
        return Duration::from_secs(10);
    }
    Duration::from_millis((elapsed_ms as f64 * (1.0 / duty - 1.0)).ceil() as u64)
}

fn bucket(seconds: f64, step: f64) -> Option<u32> {
    valid_position(seconds).then(|| (seconds / step).floor() as u32)
}

fn timeline_bucket(seconds: f64, duration: f64, step: f64) -> Option<u32> {
    let count = bucket_count(duration, step);
    if count == 0 {
        None
    } else {
        bucket(seconds, step).map(|b| b.min(count - 1))
    }
}

// React requests wake the worker immediately. Health checks remain bounded to 100 ms;
// the final, shorter portion of a duty-cycle rest is waited exactly.
fn wait_for_update(shared: &Shared, generation: u64, session: &Session, timeout: Duration) {
    let state = shared.0.lock().unwrap();
    if !state.shutdown
        && state.generation == generation
        && state.session.as_ref().is_some_and(|s| {
            s.wanted == session.wanted && s.pointer_on_timeline == session.pointer_on_timeline
        })
    {
        let _ = shared.1.wait_timeout(state, timeout).unwrap();
    }
}

fn source_current(main: &Mpv, url: &str) -> bool {
    main.get_property("path", Kind::String)
        .and_then(|v| v.as_str().map(str::to_owned))
        .as_deref()
        == Some(url)
        && main.get_property("idle-active", Kind::Flag) == Some(false.into())
}

fn source_active(main: &Mpv, url: &str) -> bool {
    source_current(main, url) && number(main, "time-pos").is_some()
}

// Demand remains available during a main-player seek/buffer stall after its first frame.
// Background work still requires current playback health and never shares these relaxed gates.
fn demand_wait_reason(current: bool, seen_frame: bool, configured: bool) -> Option<&'static str> {
    if !current {
        Some("source-inactive")
    } else if !seen_frame {
        Some("main-first-frame")
    } else if !configured {
        Some("duration-unknown")
    } else {
        None
    }
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
    source_active(main, url)
        && main.get_property("video-out-params", Kind::Json).is_some()
        && main.get_property("seeking", Kind::Flag) != Some(true.into())
        && main.get_property("paused-for-cache", Kind::Flag) != Some(true.into())
        && number(main, "demuxer-cache-duration").is_none_or(|v| v == 0.0 || v >= 1.0)
}

// A speculative seek yields to user work; a demand seek finishes and caches.
struct WaitPolicy {
    kind: WorkKind,
    require_seek: bool,
    speculative_open: bool,
}
impl WaitPolicy {
    fn new(kind: WorkKind, require_seek: bool) -> Self {
        Self {
            kind,
            require_seek,
            speculative_open: false,
        }
    }
}

#[cfg(test)]
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
    let opening = policy.kind == WorkKind::OpenFrame;
    let usenet_open = opening
        && shared
            .0
            .lock()
            .unwrap()
            .session
            .as_ref()
            .is_some_and(|s| s.source.kind == PreviewSourceKind::Usenet);
    let mut deadline = started + timeout;
    let max_deadline = if usenet_open {
        started + Duration::from_secs(15)
    } else {
        deadline
    };
    let mut extended = false;
    let mut last_input_progress = None;
    let mut last_packet_bytes = None;
    let slow_at = Instant::now() + Duration::from_secs(2);
    let mut logged_slow = false;
    let mut saw_seek = !policy.require_seek;
    while Instant::now() < max_deadline {
        if !active(shared, generation) {
            return Err("cancelled");
        }
        if opening {
            let cache = mpv.get_property("demuxer-cache-state", Kind::Json);
            let bytes = cache
                .as_ref()
                .and_then(|v| v.get("total-bytes"))
                .and_then(|v| v.as_u64());
            let rate = cache
                .as_ref()
                .and_then(|v| v.get("raw-input-rate"))
                .and_then(|v| v.as_f64());
            if bytes
                .zip(last_packet_bytes)
                .is_some_and(|(now, old)| now > old)
                || rate.is_some_and(|r| r.is_finite() && r > 0.0)
            {
                last_input_progress = Some(Instant::now());
            }
            last_packet_bytes = bytes;
            if Instant::now() >= deadline {
                let waiting = cache
                    .as_ref()
                    .and_then(|v| v.get("underrun"))
                    .and_then(|v| v.as_bool())
                    == Some(true);
                let progress =
                    last_input_progress.is_some_and(|at| at.elapsed() < Duration::from_secs(2));
                if !extended
                    && !preview_policy::open_timeout_s(
                        usenet_open,
                        started.elapsed().as_secs_f64(),
                        waiting,
                        progress,
                    )
                {
                    extended = true;
                    deadline = max_deadline;
                    let state = shared.0.lock().unwrap();
                    if let Some(s) = &state.session {
                        log::info!(target: "seek_preview", "session={} open_timeout_extended_s=15 input_progress=estimated packet_or_rate_progress=true", s.id);
                    }
                } else {
                    return Err(if extended {
                        "open-input-timeout"
                    } else {
                        "timeout"
                    });
                }
            }
        }
        if saw_seek && policy.kind == WorkKind::Background || policy.speculative_open {
            background_wait_gate(shared, main, policy.speculative_open)?;
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
    Err(if extended {
        "open-input-timeout"
    } else {
        "timeout"
    })
}

#[derive(Clone)]
struct Image {
    bucket: u32,
    position: f64,
    covers_until: f64,
    covers_from: f64,
    data: String,
    aspect_ratio: f64,
    cost_s: Option<f64>,
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
    failed_work_total_ms: u64,
    waiting: usize,
    buffering_intervals: usize,
    background_work_ms: u64,
    preempted: usize,
    demand_work_ms: u64,
    slow_demand_seeks: usize,
    rate_limit_wait_ms: u64,
    last_queue_bytes: Option<u64>,
    queue_growth_bytes: u64,
}

impl Stats {
    fn observe_queue(&mut self, mpv: &Mpv, session: &str) {
        let total = mpv
            .get_property("demuxer-cache-state", Kind::Json)
            .and_then(|v| v.get("total-bytes").and_then(|v| v.as_u64()));
        let delta = total
            .zip(self.last_queue_bytes)
            .map(|(now, before)| i128::from(now) - i128::from(before));
        if let Some(delta) = delta.filter(|v| *v > 0) {
            self.queue_growth_bytes = self.queue_growth_bytes.saturating_add(delta as u64);
        }
        self.last_queue_bytes = total;
        // Packet storage includes overhead and can be evicted or reused. Neither
        // this delta nor its positive sum is a measured network-byte counter.
        log::info!(target: "seek_preview", "session={session} demux_queue_bytes_delta={delta:?} demux_queue_growth_total_bytes={} network_bytes=unavailable traffic_measurement=queue-storage-only", self.queue_growth_bytes);
    }
}

struct Recovery {
    terminal: Option<&'static str>,
    errors: u32,
    init_errors: u32,
    failed_work_ms: u64,
    retry_at: Instant,
}
impl Recovery {
    fn new() -> Self {
        Self {
            terminal: None,
            errors: 0,
            init_errors: 0,
            failed_work_ms: 0,
            retry_at: Instant::now(),
        }
    }
    fn initialized(&mut self) {
        self.init_errors = 0;
    }
    fn success(&mut self) {
        self.errors = 0;
        self.init_errors = 0;
        self.failed_work_ms = 0;
        self.retry_at = Instant::now();
    }
    fn failure(&mut self, reason: &'static str, elapsed_ms: u64) -> Duration {
        self.errors = self.errors.saturating_add(1);
        self.failed_work_ms = self.failed_work_ms.saturating_add(elapsed_ms);
        if reason == "decoder-init" {
            self.init_errors += 1;
            // Initialization can fail transiently; stop only after three attempts.
            if self.init_errors >= 3 {
                self.terminal = Some(reason);
            }
        }
        if matches!(
            reason,
            "not-seekable"
                | "dolby-vision-profile-unsupported"
                | "dolby-vision-needs-conversion"
                | "dv-base-filter"
                | "colour-filter"
                | "p5-invalid-output"
                | "vulkan-unavailable"
                | "libplacebo-missing"
                | "libplacebo-options-unavailable"
                | "colour-filter-failed"
        ) {
            self.terminal = Some(reason);
        }
        let delay = if self.failed_work_ms >= FAILED_WORK_COOLDOWN_MS {
            Duration::from_secs(30)
        } else {
            Duration::from_millis((500_u64 << self.errors.saturating_sub(1).min(6)).min(30_000))
        };
        self.retry_at = Instant::now() + delay;
        delay
    }
    fn waiting_reason(&self) -> Option<&'static str> {
        self.terminal
            .or_else(|| (Instant::now() < self.retry_at).then_some("retry-backoff"))
    }
}

struct Decoder {
    mpv: Mpv,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.mpv.command(&["stop"]);
    }
}

pub fn usenet_background_enabled() -> bool {
    preview_policy::usenet_background_enabled()
}

pub fn http_background_enabled() -> bool {
    preview_policy::http_background_enabled()
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
        Ok("fast-seek") => "fast-seek",
        _ => "default",
    }
}

pub fn log_policy() {
    preview_policy::log_constants();
}

fn decoder_variant(kind: PreviewSourceKind) -> &'static str {
    if matches!(
        test_variant(),
        "fast-seek" | "persistent-http" | "skip-loop-filter" | "mkv-no-duration"
    ) && kind != PreviewSourceKind::Debrid
    {
        "default"
    } else {
        test_variant()
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

struct ColourEvidence {
    decoder: Option<u64>,
    hint: Option<u64>,
    gamma: Option<String>,
    matrix: Option<String>,
}

impl ColourEvidence {
    fn read(mpv: &Mpv, source: &PreviewSource) -> Self {
        let params = mpv.get_property("video-dec-params", Kind::Json);
        let field = |name| params.as_ref()?.get(name)?.as_str().map(str::to_owned);
        Self {
            decoder: video_profile(mpv),
            hint: source.dv_profile,
            gamma: field("gamma"),
            matrix: field("colormatrix"),
        }
    }

    fn hint_mismatch(&self) -> bool {
        self.decoder.is_none()
            && self.hint == Some(5)
            && self
                .matrix
                .as_deref()
                .is_some_and(|s| !matches!(s, "dolbyvision" | "unknown" | "auto"))
            && matches!(
                self.gamma.as_deref(),
                Some("pq" | "hlg" | "bt.1886" | "srgb" | "gamma2.2" | "gamma2.4" | "linear")
            )
    }

    fn profile(&self) -> Option<u64> {
        self.decoder.or(self.hint.filter(|_| !self.hint_mismatch()))
    }

    fn log_fields(&self) -> String {
        let source = if self.decoder.is_some() {
            "decoder"
        } else if self.profile().is_some() {
            "server-hint"
        } else {
            "none"
        };
        // Only allowlisted decoder values reach logs, never raw metadata or URLs.
        let gamma = self
            .gamma
            .as_deref()
            .filter(|s| {
                matches!(
                    *s,
                    "pq" | "hlg" | "bt.1886" | "srgb" | "gamma2.2" | "gamma2.4" | "linear"
                )
            })
            .unwrap_or("unknown");
        let matrix = self
            .matrix
            .as_deref()
            .filter(|s| {
                matches!(
                    *s,
                    "dolbyvision" | "bt.2020-ncl" | "bt.2020-cl" | "bt.709" | "bt.601" | "rgb"
                )
            })
            .unwrap_or("unknown");
        format!(
            "dv_profile_source={source} dv_hint={:?} dv_decoder={:?} decoded_gamma={gamma} decoded_matrix={matrix} dv_hint_mismatch={}",
            self.hint,
            self.decoder,
            self.hint_mismatch()
        )
    }
}

fn colour_plan(evidence: &ColourEvidence, source: &PreviewSource) -> PreviewColour {
    use PreviewColour::*;
    let profile = evidence.profile();
    let gamma = evidence.gamma.as_deref();
    let matrix = evidence.matrix.as_deref();
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
        // Compatible P8/P9 bases have their own decoded transfer. Do not require
        // a server range hint when the selected track already establishes it.
        match gamma {
            Some("pq") => return DvPq,
            Some("hlg") => return DvHlg,
            Some("bt.1886" | "srgb" | "gamma2.2" | "gamma2.4") => return DvSdr,
            _ => {}
        }
        return match source.colour {
            Pq | DvPq => DvPq,
            Hlg | DvHlg => DvHlg,
            Sdr | DvSdr => DvSdr,
            _ => Unknown,
        };
    }
    if matrix != Some("dolbyvision") {
        match gamma {
            Some("pq") => return Pq,
            Some("hlg") => return Hlg,
            Some("bt.1886" | "srgb" | "gamma2.2" | "gamma2.4" | "linear") => return Sdr,
            _ => {}
        }
    }
    // Unresolved decoder evidence retains the hint, including Unknown. The open
    // path handles compatible DV bases whose transfer still needs detection.
    source.colour
}

fn session_grid(
    main: &Mpv,
    source: &PreviewSource,
    costs: &Costs,
    duration: f64,
) -> (String, preview_policy::Grid) {
    let evidence = ColourEvidence::read(main, source);
    let plan = colour_plan(&evidence, source);
    let range = if evidence.profile().is_some() || evidence.matrix.as_deref() == Some("dolbyvision")
    {
        "dv"
    } else if plan == PreviewColour::Sdr {
        "sdr"
    } else {
        "hdr"
    };
    let width = number(main, "video-dec-params/w").or(source.width.map(f64::from));
    let wide = width.is_none_or(|w| w > 1920.0);
    let variant = if decoder_variant(source.kind) == "fast-seek"
        && std::env::var("AIOSTREAMS_PREVIEW_GRID_REFERENCE").as_deref() != Ok("classic")
    {
        "fast"
    } else {
        "classic"
    };
    let work = if source.mode == PreviewMode::Full {
        "background"
    } else {
        "demand"
    };
    let class = preview_policy::cost_class(source.kind, range, wide, variant, work);
    let grid = costs.grid(
        duration,
        &class,
        preview_policy::default_cost(source.kind, range, wide),
        source.kind,
    );
    (class, grid)
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
    background: bool,
) -> Result<Decoder, &'static str> {
    let variant = decoder_variant(session.source.kind);
    let main_colour = ColourEvidence::read(main, &session.source);
    let mut plan = colour_plan(&main_colour, &session.source);
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
        (
            "network-timeout",
            if session.source.kind == PreviewSourceKind::Usenet {
                "15"
            } else {
                "5"
            },
        ),
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
        "fast-seek" => {
            options.push(("stream-lavf-o", "multiple_requests=1"));
            options.push(("vd-lavc-skiploopfilter", "all"));
            options.push(("demuxer-mkv-probe-video-duration", "no"));
        }
        _ => {}
    }
    let start = Instant::now();
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
    log::info!(target: "seek_preview", "session={} opening=true test_variant={variant} colour_path={} filter_preconfigured={} dv_profile={:?} screenshot_path=memory colour_evidence=main {}", session.id, colour_name(plan), plan != PreviewColour::Unknown, main_colour.profile(), main_colour.log_fields());
    if p5 {
        log::info!(target: "seek_preview", "session={} p5_filter=attempting filter=libplacebo libplacebo_available=unknown vulkan_initialised=unknown dv_metadata_delivery=unverified", session.id);
    }
    decoder
        .mpv
        .command(&["loadfile", &session.url])
        .map_err(|_| "open-command")?;
    if let Err(reason) = wait_frame_for(
        &decoder.mpv,
        shared,
        generation,
        Duration::from_secs(5),
        WaitPolicy {
            speculative_open: background,
            ..WaitPolicy::new(WorkKind::OpenFrame, false)
        },
        Some(main),
    ) {
        if p5
            && matches!(
                reason,
                "vulkan-unavailable"
                    | "libplacebo-missing"
                    | "libplacebo-options-unavailable"
                    | "colour-filter-failed"
            )
        {
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
    if !preview_policy::seekable_for_preview(
        decoder.mpv.get_property("seekable", Kind::Flag) == Some(true.into()),
        decoder.mpv.get_property("partially-seekable", Kind::Flag) != Some(false.into()),
    ) {
        return Err("not-seekable");
    }
    let decoder_colour = ColourEvidence::read(&decoder.mpv, &session.source);
    let profile = decoder_colour.profile();
    if profile.is_some_and(|p| !matches!(p, 5 | 7..=9)) {
        return Err("dolby-vision-profile-unsupported");
    }
    let mut changed = false;
    let detected = colour_plan(&decoder_colour, &session.source);
    if detected != PreviewColour::Unknown && detected != plan {
        // Reconcile metadata with the selected decoded track before caching any frame.
        plan = PreviewColour::Unknown;
    }
    if plan == PreviewColour::Unknown {
        plan = colour_plan(&decoder_colour, &PreviewSource::default());
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
                WaitPolicy {
                    speculative_open: background,
                    ..WaitPolicy::new(WorkKind::Demand, true)
                },
                Some(main),
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
            if background {
                background_wait_gate(shared, Some(main), true)?;
            }
            if prepared.elapsed() >= limit {
                if p5 {
                    log::info!(target: "seek_preview", "session={} p5_filter=pending:output-not-ready stage=colour-ready libplacebo_usable=unknown", session.id);
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
                    log::info!(target: "seek_preview", "session={} p5_filter=unverified stage=colour-ready decoder_failed=decode-error libplacebo_usable=unknown", session.id);
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
    log::info!(target: "seek_preview", "session={} opened_ms={} reopened={reopened} hdr={} threads={threads} test_variant={variant} colour_path={} dv_profile={profile:?} filter_changed_after_load={changed} colour_evidence=preview {}", session.id, start.elapsed().as_millis(), plan != PreviewColour::Sdr, colour_name(plan), decoder_colour.log_fields());
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
    let cold_seek = !open_frame && in_buffered_range(&decoder.mpv, wanted_s) != Some(true);
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
                speculative_open: false,
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
    log::info!(target: "seek_preview", "session={} bucket={} target_s={wanted_s:.3} sampled_s={position:.3} covers_from_s={:.3} covers_until_s={:.3} offset_s={:.3} demand={} seek_ms={seek_ms} network_wait_ms=unavailable decode_ms=unavailable seek_phase_metrics=not-exposed capture_ms={} screenshot_ms={screenshot_ms} screenshot_path=memory jpeg_encode_ms={jpeg_encode_ms:.3} image_width={} image_height={} base64_ms={base64_ms:.3} image_bytes={} playhead_distance_s={:?} main_buffered={main_buffered:?} reopened={reopened} vf_width={width:?} vf_height={height:?} aspect_ratio={aspect:.5} output_transfer={transfer} open_frame={open_frame} demux_queue_total_bytes={queue_total:?} demux_queue_forward_bytes={queue_forward:?} raw_input_bytes_per_second={raw_rate:?}", session.id, target, wanted_s.min(position), wanted_s.max(position), position - wanted_s, demand, screenshot_at.elapsed().as_millis(), raw.width, raw.height, bytes.len(), playhead.map(|v| wanted_s-v));
    Ok(Image {
        bucket: target,
        position,
        covers_until: wanted_s.max(position),
        covers_from: wanted_s.min(position),
        data,
        aspect_ratio: aspect,
        cost_s: cold_seek.then(|| started.elapsed().as_secs_f64()),
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
    let tolerance = tolerance + COVERAGE_EPSILON_S;
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
    // Floating-point division can turn an exact slot grid slightly above its integer count.
    ((duration.clamp(0.0, 86400.0) / step).ceil() as u32).min(MAX_GRID_SLOTS)
}

fn covered_buckets(cache: &VecDeque<Image>, duration: f64, step: f64) -> BTreeSet<u32> {
    // Cover the entire bucket, not only its centre. An opening frame can cover the
    // centre while leaving an edge uncached, and two adjacent images can fill it.
    let mut ranges: Vec<_> = cache
        .iter()
        .map(|image| {
            (
                image.covers_from - step / 2.0 - COVERAGE_EPSILON_S,
                image.covers_until + step / 2.0 + COVERAGE_EPSILON_S,
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

#[derive(Default)]
struct Frontier {
    queue: VecDeque<u32>,
    turn: usize,
    playhead: Option<u32>,
}

impl Frontier {
    fn next(&mut self, have: &BTreeSet<u32>, playhead: u32, last: u32) -> Option<u32> {
        let playhead = playhead.min(last);
        if self.playhead.is_some_and(|old| old.abs_diff(playhead) >= 4) {
            self.queue.clear();
        }
        self.playhead = Some(playhead);
        loop {
            while let Some(next) = self.queue.pop_front() {
                if !have.contains(&next) {
                    return Some(next);
                }
            }
            let ahead: Vec<_> = (playhead..=last)
                .filter(|b| !have.contains(b))
                .take(4)
                .collect();
            let mut behind: Vec<_> = (0..playhead)
                .rev()
                .filter(|b| !have.contains(b))
                .take(4)
                .collect();
            behind.reverse(); // Read this backward frontier in forward order.
            let prefer_behind = self.turn % 3 == 2;
            let batch = if prefer_behind {
                if behind.is_empty() { ahead } else { behind }
            } else if ahead.is_empty() {
                behind
            } else {
                ahead
            };
            if batch.is_empty() {
                return None;
            }
            self.queue = batch.into();
            self.turn += 1;
        }
    }
}

fn finish_request(shared: &Shared, generation: u64, target: u32, duration: f64, step: f64) {
    let mut state = shared.0.lock().unwrap();
    if state.generation == generation
        && let Some(s) = state.session.as_mut()
        && s.wanted.and_then(|s| timeline_bucket(s, duration, step)) == Some(target)
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

fn log_coverage(
    session: &Session,
    cache: &VecDeque<Image>,
    duration: f64,
    position: f64,
    step: f64,
    last_cached: &mut u64,
) {
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
    let from15 = bucket((position - 900.0).max(0.0), step).unwrap_or(0);
    let through15 = bucket((position + 900.0).min((duration - 0.05).max(0.0)), step)
        .unwrap_or(0)
        .min(count - 1);
    let holes15 = (from15..=through15).filter(|b| !have.contains(b)).count();
    let cached_total = session.cached_displays.load(Ordering::Relaxed);
    let cached_interval = cached_total.saturating_sub(*last_cached);
    *last_cached = cached_total;
    log::info!(target: "seek_preview", "session={} coverage_pct={:.1} covered_buckets={} total_buckets={count} holes_within_5min_of_playhead={holes} holes_within_15min_of_playhead={holes15} playhead_s={position:.3} cached_images={} cache_encoded_bytes={} cached_displays_interval={cached_interval} cached_displays_total={cached_total}", session.id, 100.0 * have.len() as f64 / f64::from(count), have.len(), cache.len(), cache.iter().map(|i| i.data.len()).sum::<usize>());
}

fn run(library: &Path, main: &Mpv, emit: &Emit, shared: &Shared, data_dir: Option<&Path>) {
    let mut costs = Costs::load(data_dir);
    let forced_unavailable =
        std::env::var("AIOSTREAMS_PREVIEW_TEST_UNAVAILABLE").as_deref() == Ok("on");
    let learn_costs = test_variant() == "default"
        && !forced_unavailable
        && std::env::var("AIOSTREAMS_PREVIEW_COST_LEARNING").as_deref() != Ok("off")
        && std::env::var("AIOSTREAMS_PREVIEW_GRID_REFERENCE").is_err();
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
        let mut recovery = Recovery::new();
        if forced_unavailable {
            recovery.terminal = Some("test-unavailable");
            log::info!(target: "seek_preview", "session={} test_forced_unavailable=true preview_decoder_open=false main_playback_unchanged=true", session.id);
        }
        let mut last_work = Instant::now();
        let clock = Instant::now();
        let mut controller =
            Controller::with_ceiling(preview_policy::configured_ceiling(session.source.kind));
        let mut grid_class = String::new();
        let mut frontier = Frontier::default();
        let mut seen_main_frame = false;
        let mut first_main_frame_at: Option<Instant> = None;
        let mut waiting_reason = "";
        let mut last_cached = 0;
        let mut status = "";
        let mut sent_reason = "";
        let mut sent_terminal = false;
        let mut background_status = "";
        let mut step = 5.0;
        let mut configured = false;
        let mut duration_s = 0.0;
        let mut playhead_s = 0.0;
        let mut fresh_open = false;
        let mut sent_aspect = None;
        let mut last_speculative: Option<(Instant, u64)> = None;
        let mut last_health_at = 0.0;
        let mut last_rate_limited = false;
        let mut rate_governed = false;
        let mut last_gate_fail = None;
        let mut cache_full = false;
        let mut coverage_at = Instant::now();
        let mut first_wait_reason = "";
        let mut main_buffer_min: Option<f64> = None;
        let kind = preview_policy::kind_name(session.source.kind);
        let full = session.source.mode == PreviewMode::Full;
        let mode = if full { "full" } else { "demand-only" };
        log::info!(target: "seek_preview", "session={} start source_kind={kind} preview_mode={mode} grid_slot_limit={MAX_GRID_SLOTS} grid_learning={learn_costs} max_background_work_ms={MAX_BACKGROUND_WORK_MS} demand_work_budget=none max_images={MAX_IMAGES} cache_encoded_limit_bytes={MAX_CACHE_BYTES} min_background_buffer_s=10 background_duty_limit={} background_seek_cap_per_min={} provider_connections=unavailable server_read_ahead=unavailable pipeline_depth=unavailable background_input_budget=none network_bytes=unavailable test_variant={} effective_decoder_variant={} colour_path={} idle_close_s={IDLE_CLOSE_S}", session.id, preview_policy::configured_ceiling(session.source.kind), if preview_policy::conservative_source(session.source.kind) { preview_policy::seek_cap(preview_policy::configured_ceiling(session.source.kind)).to_string() } else { "none".into() }, test_variant(), decoder_variant(session.source.kind), colour_name(session.source.colour));
        while active(shared, generation) {
            let calm = healthy(main, &session.url);
            let current_source = source_current(main, &session.url);
            seen_main_frame |= current_source
                && source_active(main, &session.url)
                && main.get_property("video-out-params", Kind::Json).is_some();
            if seen_main_frame {
                first_main_frame_at.get_or_insert_with(Instant::now);
            }
            if !configured
                && !forced_unavailable
                && seen_main_frame
                && (number(main, "duration").is_some_and(|s| s > 86400.0)
                    || first_main_frame_at
                        .is_some_and(|at| at.elapsed() >= Duration::from_secs(15)))
            {
                recovery.terminal = Some("duration-unsupported");
            }
            if !configured
                && seen_main_frame
                && source_active(main, &session.url)
                && let Some(duration) =
                    number(main, "duration").filter(|s| *s > 0.0 && *s <= 86400.0)
            {
                let (class, grid) = session_grid(main, &session.source, &costs, duration);
                grid_class = class;
                step = grid.step;
                log::info!(target: "seek_preview", "session={} grid_slots={} grid_budget_slots={} grid_cost_s={:.3} grid_cost_source={} grid_class={grid_class} grid_samples={} grid_expected_duty={} grid_history_version=2 grid_frozen=true", session.id, bucket_count(duration, grid.step), grid.slots, grid.cost_s, if grid.learned { "learned" } else { "default" }, grid.samples, preview_policy::expected_duty(session.source.kind));
                duration_s = duration;
                configured = true;
                if recovery.terminal == Some("duration-unsupported") {
                    recovery.terminal = None;
                }
                status = "";
                log::info!(target: "seek_preview", "session={} step_ms={} duration_s={duration:.3} total_buckets={} grid_slot_limit={MAX_GRID_SLOTS}", session.id, (step * 1000.0).round() as u32, bucket_count(duration, step));
            }
            if source_active(main, &session.url) {
                playhead_s = number(main, "time-pos").unwrap_or(playhead_s);
            }
            let gate_reason = demand_wait_reason(current_source, seen_main_frame, configured);
            let reason = gate_reason
                .or_else(|| recovery.waiting_reason())
                .unwrap_or("none");
            let next_status = if recovery.terminal.is_some()
                || (recovery.errors >= 3 && reason == "retry-backoff")
            {
                "unavailable"
            } else if reason != "none" {
                "waiting"
            } else {
                "ready"
            };
            if reason != waiting_reason {
                waiting_reason = reason;
                log::info!(target: "seek_preview", "session={} waiting_reason={reason} source_current={current_source} first_main_frame_seen={seen_main_frame} main_seeking={:?} main_buffering={:?} main_buffer_ahead_s={:?} consecutive_errors={} failed_work_since_success_ms={}", session.id, main.get_property("seeking", Kind::Flag), main.get_property("paused-for-cache", Kind::Flag), number(main, "demuxer-cache-duration"), recovery.errors, recovery.failed_work_ms);
            }
            let aspect = aspect_ratio(main, "video-params");
            if status != next_status
                || aspect != sent_aspect
                || sent_reason != reason
                || sent_terminal != recovery.terminal.is_some()
            {
                if status != next_status && next_status == "waiting" {
                    stats.waiting += 1;
                }
                status = next_status;
                sent_reason = reason;
                sent_terminal = recovery.terminal.is_some();
                sent_aspect = aspect;
                emit(Outbound::PreviewStatus {
                    session: session.id.clone(),
                    state: status.into(),
                    reason: reason.into(),
                    terminal: recovery.terminal.is_some(),
                    step_ms: (step * 1000.0).round() as u32,
                    aspect_ratio: aspect,
                });
                log::info!(target: "seek_preview", "session={} state={status} waiting_reason={reason} decoder_open={} aspect_ratio={aspect:?}", session.id, decoder.is_some());
            }
            if decoder.is_some() && last_work.elapsed() > Duration::from_secs(IDLE_CLOSE_S) {
                decoder = None;
                log::info!(target: "seek_preview", "session={} decoder_closed=idle", session.id);
            }
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
            if current_source && let Some(seconds) = buffered {
                main_buffer_min = Some(main_buffer_min.map_or(seconds, |old| old.min(seconds)));
            }
            let paused = main.get_property("pause", Kind::Flag) == Some(true.into());
            let seeking = main.get_property("seeking", Kind::Flag) == Some(true.into());
            let buffering = main.get_property("paused-for-cache", Kind::Flag) == Some(true.into());
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
            let decision = controller.update(
                Health {
                    at: clock.elapsed().as_secs_f64(),
                    buffered,
                    position: playhead_s,
                    speed: number(main, "speed").unwrap_or(1.0),
                    paused,
                    seeking,
                    user_seek: shared
                        .0
                        .lock()
                        .unwrap()
                        .last_main_seek
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(5)),
                    buffering,
                    underrun: cache_state
                        .as_ref()
                        .and_then(|v| v.get("underrun"))
                        .and_then(|v| v.as_bool())
                        == Some(true),
                    reader_idle,
                    pointer: current.pointer_on_timeline,
                    current: current_source && seen_main_frame,
                },
                session.source.kind,
                full && !forced_unavailable,
            );
            let health_at = clock.elapsed().as_secs_f64();
            if last_rate_limited {
                stats.rate_limit_wait_ms +=
                    ((health_at - last_health_at).clamp(0.0, 10.0) * 1000.0) as u64;
            }
            last_health_at = health_at;
            last_rate_limited = decision.reason.ends_with("rate-limit");
            rate_governed |= last_rate_limited;
            let duty = decision.value;
            let throttled = decision.throttled;
            if let Some(cause) = decision.cause {
                if buffering {
                    stats.buffering_intervals += 1;
                }
                log::info!(target: "seek_preview", "session={} playback_buffering={buffering} buffering_cause={} duty_at_buffering={:?} background_throttle_s={} duty_level={}", session.id, cause.name(), decision.at_buffering, if throttled { 60 } else { 0 }, decision.level);
            }
            let background_ok = duty > 0.0;
            let bg_state = if stats.background_work_ms >= MAX_BACKGROUND_WORK_MS {
                "work-budget"
            } else if cache_full {
                "cache-size-limit"
            } else {
                decision.reason
            };
            // Once the rolling cap binds, aggregate routine allowed/rate-limited
            // transitions. Real health/pointer gates still log immediately.
            let logged_bg_state = if rate_governed
                && matches!(
                    bg_state,
                    "stable" | "paused" | "drop" | "usenet-rate-limit" | "http-rate-limit"
                ) {
                "rate-governed"
            } else {
                bg_state
            };
            if logged_bg_state != background_status || last_gate_fail != decision.gate_fail {
                background_status = logged_bg_state;
                last_gate_fail = decision.gate_fail;
                log::info!(target: "seek_preview", "session={} background_state={logged_bg_state} paused={paused} main_buffer_ahead_s={buffered:?} main_reader_idle={reader_idle} background_duty={duty} duty_level={} duty_reason={} duty_ceiling={} background_gate_fail={:?} background_throttled={throttled} background_work_ms={} network_bytes=unavailable", session.id, decision.level, decision.reason, decision.ceiling, decision.gate_fail, stats.background_work_ms);
            }
            if configured && coverage_at.elapsed() >= Duration::from_secs(30) {
                log_coverage(
                    &session,
                    &cache,
                    duration_s,
                    playhead_s,
                    step,
                    &mut last_cached,
                );
                log::info!(target: "seek_preview", "session={} main_buffer_min_s={main_buffer_min:?} main_buffer_trailing_30s={:?} main_buffer_trailing_60s={:?} main_buffer_ahead_s={buffered:?} paused={paused} main_reader_idle={reader_idle} background_duty={duty} duty_level={} duty_reason={} duty_ceiling={} background_gate_fail={:?} background_throttled={throttled}", session.id, controller.minimum(clock.elapsed().as_secs_f64(), 30.0), controller.minimum(clock.elapsed().as_secs_f64(), 60.0), decision.level, decision.reason, decision.ceiling, decision.gate_fail);
                main_buffer_min = None;
                costs.save();
                coverage_at = Instant::now();
            }
            if let Some(first_hover_at) = current.first_hover_at
                && !current.first_display_reported
            {
                let reason = if reason != "none" {
                    reason
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
            if gate_reason.is_some() {
                wait_for_update(shared, generation, &current, Duration::from_millis(100));
                continue;
            }
            let wanted_seconds = current.wanted;
            let wanted =
                wanted_seconds.and_then(|seconds| timeline_bucket(seconds, duration_s, step));
            if let Some(target) = wanted
                && let Some(image) = nearby(&cache, wanted_seconds.unwrap(), step / 2.0)
            {
                stats.hits += 1;
                deliver(emit, &session, image, true, 0);
                finish_request(shared, generation, target, duration_s, step);
                continue;
            }
            if recovery.waiting_reason().is_some() {
                wait_for_update(shared, generation, &current, Duration::from_millis(100));
                continue;
            }
            let opening_on_demand = wanted.is_some();
            let preload =
                if !full || cache_full || stats.background_work_ms >= MAX_BACKGROUND_WORK_MS {
                    false
                } else if stats.opens == 0 {
                    if session.source.kind == PreviewSourceKind::Usenet {
                        // A bounded warm open may precede the 60-second scan gate.
                        calm && reader_idle
                            && buffered
                                .is_some_and(|s| s >= preview_policy::CONSERVATIVE_ABORT_BUFFER_S)
                            && !current.pointer_on_timeline
                    } else if session.source.kind == PreviewSourceKind::Http {
                        background_ok
                    } else {
                        calm && buffered.is_some_and(|s| s >= 3.0) && !current.pointer_on_timeline
                    }
                } else if background_ok
                    && !cache_full
                    && stats.background_work_ms < MAX_BACKGROUND_WORK_MS
                {
                    let duration = number(main, "duration").unwrap_or(0.0);
                    let count = bucket_count(duration, step);
                    count > 0 && covered_buckets(&cache, duration, step).len() < count as usize
                } else {
                    false
                };
            if decoder.is_none() && (opening_on_demand || preload) {
                let opening_at = Instant::now();
                let result = open(
                    library,
                    main,
                    &session,
                    shared,
                    generation,
                    stats.opens > 0,
                    !opening_on_demand,
                );
                if !opening_on_demand {
                    let elapsed = opening_at.elapsed().as_millis() as u64;
                    stats.background_work_ms += elapsed;
                    last_speculative = Some((Instant::now(), elapsed));
                }
                match result {
                    Ok(d) => {
                        recovery.initialized();
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
                                target: timeline_bucket(position, duration_s, step).unwrap_or(0),
                                kind: WorkKind::OpenFrame,
                                step,
                                reopened: fresh_open,
                            },
                        ) {
                            Ok(image) if active(shared, generation) => {
                                // A cached playhead frame on reopen must not clear a
                                // repeatedly failing, distant demand seek's cooldown.
                                if wanted_seconds.is_none_or(|p| {
                                    p >= image.covers_from - step / 2.0
                                        && p <= image.covers_until + step / 2.0
                                }) {
                                    recovery.success();
                                }
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
                                let elapsed = opened_at.elapsed().as_millis() as u64;
                                stats.failed_work_total_ms += elapsed;
                                recovery.failure(reason, elapsed);
                            }
                        }
                        stats.last_queue_bytes = None;
                        stats.observe_queue(&d.mpv, &session.id);
                        decoder = Some(d);
                        last_work = Instant::now();
                    }
                    Err("cancelled") => break,
                    Err("superseded-background" | "playback-busy") => {
                        stats.preempted += 1;
                    }
                    Err(reason) => {
                        let logged_reason = if reason == "open-input-timeout" {
                            "timeout"
                        } else {
                            reason
                        };
                        log::info!(target: "seek_preview", "session={} open_failed={logged_reason} stage=open extended_input_timeout={}", session.id, reason == "open-input-timeout");
                        stats.errors += 1;
                        stats.failed_work_total_ms += opening_at.elapsed().as_millis() as u64;
                        let delay =
                            recovery.failure(reason, opening_at.elapsed().as_millis() as u64);
                        log::info!(target: "seek_preview", "session={} retry_delay_ms={} terminal_failure={:?}", session.id, delay.as_millis(), recovery.terminal);
                    }
                }
                continue;
            }
            let mut kind = WorkKind::Demand;
            let target = wanted.or_else(|| {
                if stats.background_work_ms >= MAX_BACKGROUND_WORK_MS
                    || cache_full
                    || last_speculative
                        .is_some_and(|(at, elapsed)| at.elapsed() < speculative_rest(elapsed, duty))
                {
                    return None;
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
                let next = frontier.next(&have, playhead, count - 1)?;
                kind = WorkKind::Background;
                Some(next)
            });
            let Some(target) = target else {
                let now = Instant::now();
                let rest_until =
                    last_speculative.map(|(at, elapsed)| at + speculative_rest(elapsed, duty));
                let deadline = rest_until.unwrap_or(now);
                let wait = if background_ok && deadline > now {
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(100))
                } else {
                    Duration::from_millis(100)
                };
                wait_for_update(shared, generation, &current, wait);
                continue;
            };
            let demand = kind == WorkKind::Demand;
            if decoder.is_none() {
                wait_for_update(shared, generation, &current, Duration::from_millis(100));
                continue;
            }
            let started = Instant::now();
            stats.attempts += 1;
            if !demand {
                stats.background += 1;
                if preview_policy::conservative_source(session.source.kind) {
                    controller.background_attempt(clock.elapsed().as_secs_f64());
                }
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
            if demand && elapsed > 2000 {
                stats.slow_demand_seeks += 1;
                log::info!(target: "seek_preview", "session={} demand_seek_over_2s=true demand_seek_elapsed_ms={elapsed} demand_seek_result={} slow_demand_seeks={}", session.id, result.as_ref().err().copied().unwrap_or("image"), stats.slow_demand_seeks);
            }
            stats.observe_queue(&decoder.as_ref().unwrap().mpv, &session.id);
            if !demand {
                stats.background_work_ms += elapsed;
                last_speculative = Some((Instant::now(), elapsed));
            }
            if let Err(reason) = result.as_ref() {
                // Expected speculative interruptions already spend the background budget.
                // They must not disable later demand work through the failure guard.
                if !matches!(
                    *reason,
                    "cancelled" | "superseded-background" | "playback-busy"
                ) {
                    stats.failed_work_total_ms += elapsed;
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
                    recovery.success();
                    if let Some(cost_s) = image.cost_s.filter(|_| {
                        learn_costs
                            && (full && kind == WorkKind::Background
                                || !full && kind == WorkKind::Demand)
                    }) {
                        costs.observe(&grid_class, cost_s);
                    }
                    if demand {
                        stats.demand_work_ms += elapsed;
                    }
                    let newest = shared
                        .0
                        .lock()
                        .unwrap()
                        .session
                        .as_ref()
                        .and_then(|s| s.wanted);
                    if demand
                        && newest.and_then(|s| timeline_bucket(s, duration_s, step)) != Some(target)
                    {
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
                    fresh_open = false;
                }
                Err("cancelled") => break,
                Err("playback-busy") => {
                    stats.preempted += 1;
                    decoder = None; // Close the in-flight reader, rather than merely stopping new seeks.
                }
                Err("superseded-background") => {
                    stats.preempted += 1;
                }
                Err(reason) => {
                    stats.errors += 1;
                    let delay = recovery.failure(reason, elapsed);
                    // A timed-out/failed seek can leave the private decoder stuck. Reopen it
                    // after backoff; never seek, stop or reset the main player.
                    decoder = None;
                    log::info!(target: "seek_preview", "session={} bucket={target} failed={reason} retry_delay_ms={} terminal_failure={:?}", session.id, delay.as_millis(), recovery.terminal);
                }
            }
            last_work = Instant::now();
            if demand && completed {
                finish_request(shared, generation, target, duration_s, step);
            }
            // The duty-cycle rest is the only routine delay. Buffer stalls use
            // their own pause/throttle; retries use Recovery's separate deadline.
        }
        drop(decoder);
        costs.save();
        log_coverage(
            &session,
            &cache,
            duration_s,
            playhead_s,
            step,
            &mut last_cached,
        );
        log::info!(target: "seek_preview", "session={} summary source_kind={kind} preview_mode={mode} generated={} background_candidates={} background_work_ms={} cached_displays_total={} preempted={} native_hits={} errors={} completed_after_move={} opens={} readiness_wait_intervals={} main_buffering_intervals={} attempts={} work_ms={} failed_work_total_ms={} demand_work_ms={} slow_demand_seeks={} rate_limit_wait_ms={} demux_queue_growth_total_bytes={} network_bytes=unavailable", session.id, stats.generated, stats.background, stats.background_work_ms, session.cached_displays.load(Ordering::Relaxed), stats.preempted, stats.hits, stats.errors, stats.completed_after_move, stats.opens, stats.waiting, stats.buffering_intervals, stats.attempts, stats.work_ms, stats.failed_work_total_ms, stats.demand_work_ms, stats.slow_demand_seeks, stats.rate_limit_wait_ms, stats.queue_growth_bytes);
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
    static FAILED_COUNT: AtomicUsize = AtomicUsize::new(0);
    static USENET_BACKGROUND_COUNT: AtomicUsize = AtomicUsize::new(0);
    static HTTP_BACKGROUND_COUNT: AtomicUsize = AtomicUsize::new(0);
    static BACKGROUND_CAPTURE_COUNT: AtomicUsize = AtomicUsize::new(0);
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
                if message.contains("open_failed=") {
                    FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                if message.contains("session=fixture-usenet")
                    && message.contains("seek_issued=true demand=false")
                {
                    USENET_BACKGROUND_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                if message.contains("session=fixture-http")
                    && message.contains("seek_issued=true demand=false")
                {
                    HTTP_BACKGROUND_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                if (message.contains("session=fixture-http-background")
                    || message.contains("session=fixture-usenet-background"))
                    && message.contains("demand=false seek_ms=")
                    && message.contains("open_frame=false")
                {
                    BACKGROUND_CAPTURE_COUNT.fetch_add(1, Ordering::Relaxed);
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

    fn step_for(duration: f64) -> f64 {
        preview_policy::grid_for(duration, 0.35, false, PreviewSourceKind::Debrid).step
    }

    #[test]
    fn duration_based_grid_and_sample_time_cache() {
        assert_eq!(step_for(600.0), 5.0);
        assert_eq!(step_for(1440.0), 5.0);
        assert_eq!(step_for(1800.0), 5.0);
        assert_eq!(step_for(3600.0), 6.0);
        assert_eq!(step_for(7200.0), 12.0);
        assert_eq!(step_for(14400.0), 24.0);
        assert_eq!(step_for(86400.0), 144.0);
        assert_eq!(step_for(0.0), 5.0);
        assert_eq!(step_for(f64::NAN), 5.0);
        for duration in [0.1, 1200.001, 1200.96, 1440.090, 13691.758, 86400.0] {
            assert!(bucket_count(duration, step_for(duration)) <= MAX_GRID_SLOTS);
            assert_eq!(
                timeline_bucket(duration, duration, step_for(duration)),
                Some(bucket_count(duration, step_for(duration)) - 1)
            );
        }
        let cache = VecDeque::from([Image {
            bucket: 2,
            position: 7.0,
            covers_until: 25.0,
            covers_from: 7.0,
            data: String::new(),
            aspect_ratio: 16.0 / 9.0,
            cost_s: None,
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
            first_hover_at: None,
            first_display_reported: false,
            cached_displays: Arc::default(),
        };
        assert!(!should_yield(&session, WorkKind::Background));
        session.pointer_on_timeline = true;
        assert!(should_yield(&session, WorkKind::Background));
        session.wanted = Some(10.0);
        assert!(!should_yield(&session, WorkKind::Demand));
        assert_eq!(speculative_rest(231, 0.5).as_millis(), 231);
        assert_eq!(speculative_rest(231, 0.25).as_millis(), 693);
        assert_eq!(speculative_rest(231, 0.8).as_millis(), 58);
    }

    #[test]
    fn transient_failures_recover_and_only_unsupported_paths_are_terminal() {
        let mut recovery = Recovery::new();
        for _ in 0..12 {
            recovery.failure("timeout", 8_021);
        }
        assert!(recovery.terminal.is_none());
        assert_eq!(recovery.failure("decode-error", 1), Duration::from_secs(30));
        recovery.success();
        assert_eq!(recovery.failed_work_ms, 0);
        assert_eq!(recovery.errors, 0);
        assert!(recovery.waiting_reason().is_none());
        assert_eq!(
            recovery.failure("seek-command", 1),
            Duration::from_millis(500)
        );
        recovery.failure("libplacebo-missing", 1);
        assert_eq!(recovery.terminal, Some("libplacebo-missing"));
        let mut init = Recovery::new();
        init.failure("decoder-init", 1);
        init.failure("decoder-init", 1);
        assert!(init.terminal.is_none());
        let failed_work = init.failed_work_ms;
        init.initialized();
        assert_eq!(init.init_errors, 0);
        assert_eq!(init.failed_work_ms, failed_work);
        for _ in 0..3 {
            init.failure("decoder-init", 1);
        }
        assert_eq!(init.terminal, Some("decoder-init"));
    }

    #[test]
    fn decoder_colour_overrides_false_p5_hints_without_bypassing_real_p5() {
        let hint = PreviewSource {
            colour: PreviewColour::Dv,
            dv_profile: Some(5),
            ..PreviewSource::default()
        };
        let evidence = |decoder, gamma: &str, matrix: &str| ColourEvidence {
            decoder,
            hint: Some(5),
            gamma: Some(gamma.into()),
            matrix: Some(matrix.into()),
        };
        let hdr10 = evidence(None, "pq", "bt.2020-ncl");
        assert!(hdr10.hint_mismatch());
        assert!(hdr10.log_fields().contains("dv_profile_source=none"));
        assert!(!evidence(None, "unknown", "bt.2020-ncl").hint_mismatch());
        assert!(!evidence(None, "pq", "unknown").hint_mismatch());
        assert_eq!(colour_plan(&hdr10, &hint), PreviewColour::Pq);
        assert_eq!(
            colour_plan(&evidence(None, "bt.1886", "bt.709"), &hint),
            PreviewColour::Sdr
        );
        assert_eq!(
            colour_plan(&evidence(Some(5), "pq", "bt.2020-ncl"), &hint),
            PreviewColour::Dv
        );
        assert_eq!(
            colour_plan(&evidence(None, "pq", "dolbyvision"), &hint),
            PreviewColour::Dv
        );
        assert_eq!(
            colour_plan(&evidence(Some(7), "pq", "dolbyvision"), &hint),
            PreviewColour::DvPq
        );
        let pending = ColourEvidence {
            decoder: None,
            hint: Some(5),
            gamma: None,
            matrix: None,
        };
        assert!(!pending.hint_mismatch());
        assert_eq!(colour_plan(&pending, &hint), PreviewColour::Dv);
        for (gamma, expected) in [
            ("pq", PreviewColour::DvPq),
            ("hlg", PreviewColour::DvHlg),
            ("bt.1886", PreviewColour::DvSdr),
        ] {
            let p8 = ColourEvidence {
                decoder: Some(8),
                hint: None,
                gamma: Some(gamma.into()),
                matrix: Some("dolbyvision".into()),
            };
            assert_eq!(colour_plan(&p8, &PreviewSource::default()), expected);
        }
    }

    #[test]
    fn first_hover_timer_requires_an_accepted_request_and_resets_on_leave() {
        let worker = Previews {
            shared: Arc::default(),
            thread: None,
        };
        worker.start_with_source(
            "hover".into(),
            "fixture".into(),
            PreviewSource {
                kind: PreviewSourceKind::Http,
                ..PreviewSource::default()
            },
        );
        worker.hover("hover", true);
        assert!(
            worker
                .shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .first_hover_at
                .is_none()
        );
        worker.hover("hover", false);
        worker.hover("hover", true);
        worker.request("hover", Some(10.0));
        assert!(
            worker
                .shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .first_hover_at
                .is_some()
        );
        worker.hover("hover", false);
        assert!(
            worker
                .shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .first_hover_at
                .is_none()
        );
        worker.request("hover", Some(20.0));
        worker.report_display("hover", "display-cold");
        assert!(
            worker
                .shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .first_display_reported
        );
    }

    #[test]
    fn forward_batches_fill_every_slot_across_moving_playheads() {
        let mut frontier = Frontier::default();
        let mut have = BTreeSet::new();
        for turn in 0..600 {
            let playhead = (turn / 20).min(599);
            let b = frontier.next(&have, playhead, 599).unwrap();
            assert!(have.insert(b));
        }
        assert_eq!(have.len(), 600);
        assert_eq!(frontier.next(&have, 599, 599), None);
        let mut frontier = Frontier::default();
        let mut have = BTreeSet::new();
        let batch: Vec<_> = (0..12)
            .map(|_| {
                let next = frontier.next(&have, 20, 30).unwrap();
                have.insert(next);
                next
            })
            .collect();
        assert_eq!(batch, vec![20, 21, 22, 23, 24, 25, 26, 27, 16, 17, 18, 19]);
    }

    #[test]
    fn native_mode_respects_explicit_demand_only_and_off() {
        let worker = Previews {
            shared: Arc::default(),
            thread: None,
        };
        worker.start_with_source(
            "usenet".into(),
            "fixture".into(),
            PreviewSource {
                mode: PreviewMode::DemandOnly,
                kind: PreviewSourceKind::Usenet,
                ..PreviewSource::default()
            },
        );
        assert_eq!(
            worker
                .shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .source
                .mode,
            PreviewMode::DemandOnly
        );
        worker.start_with_source(
            "off".into(),
            "fixture".into(),
            PreviewSource {
                mode: PreviewMode::Off,
                ..PreviewSource::default()
            },
        );
        assert!(worker.shared.0.lock().unwrap().session.is_none());
    }

    #[test]
    fn long_grid_fits_observed_lotr_size_and_still_enforces_encoded_cap() {
        let mut cache = VecDeque::new();
        let step = preview_policy::grid_for(13691.758, 0.78, true, PreviewSourceKind::Debrid).step;
        for b in 0..bucket_count(13691.758, step) {
            keep_image(
                &mut cache,
                Image {
                    bucket: b,
                    position: (f64::from(b) + 0.5) * step,
                    covers_from: (f64::from(b) + 0.5) * step,
                    covers_until: (f64::from(b) + 0.5) * step,
                    data: "x".repeat(8014),
                    aspect_ratio: 2.0,
                    cost_s: None,
                },
            );
        }
        assert_eq!(cache.len(), 400);
        assert_eq!(covered_buckets(&cache, 13691.758, step).len(), 400);
        for b in 0..400 {
            assert!(nearby(&cache, f64::from(b) * step, step / 2.0).is_some());
        }
        let mut large = cache[0].clone();
        large.bucket = 240;
        large.data = "x".repeat(MAX_CACHE_BYTES);
        assert!(!room_for_image(&cache, &large));
        keep_image(&mut cache, large);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].data.len(), MAX_CACHE_BYTES);
    }

    #[test]
    fn demand_needs_a_first_frame_and_current_source_but_not_a_calm_player() {
        assert_eq!(
            demand_wait_reason(true, false, true),
            Some("main-first-frame")
        );
        assert_eq!(
            demand_wait_reason(false, true, true),
            Some("source-inactive")
        );
        assert_eq!(
            demand_wait_reason(true, true, false),
            Some("duration-unknown")
        );
        assert_eq!(demand_wait_reason(true, true, true), None);
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
            cost_s: None,
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
        assert_eq!(Frontier::default().next(&have, 2, 2), Some(1));
    }

    #[test]
    fn frontier_refills_evicted_slots_even_as_playhead_moves() {
        let mut have = BTreeSet::new();
        let mut frontier = Frontier::default();
        for turn in 0..143 {
            let moving_playhead = (turn / 5).min(142) as u32;
            let b = frontier.next(&have, moving_playhead, 142).unwrap();
            assert!(have.insert(b), "scheduler repeated an existing slot");
        }
        assert_eq!(have.len(), 143);
        assert_eq!(frontier.next(&have, 100, 142), None);
        have.remove(&60);
        assert_eq!(frontier.next(&have, 100, 142), Some(60));
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
            ..PreviewSource::default()
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
            first_hover_at: None,
            first_display_reported: false,
            cached_displays: Arc::default(),
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
        match open(&library, &main, &session, &shared, 1, false, false) {
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
        let fixture_source = PreviewSource {
            kind: PreviewSourceKind::Debrid,
            colour: match std::env::var("PREVIEW_TEST_EXPECT_GAMMA").as_deref() {
                Ok("pq") => PreviewColour::Pq,
                Ok("hlg") => PreviewColour::Hlg,
                _ => PreviewColour::Sdr,
            },
            dv_profile: std::env::var("PREVIEW_TEST_DV_HINT")
                .ok()
                .and_then(|v| v.parse().ok()),
            ..PreviewSource::default()
        };
        let shared: Shared = Arc::default();
        {
            let mut s = shared.0.lock().unwrap();
            s.generation = 1;
            s.session = Some(Session {
                id: "test".into(),
                url: url.clone(),
                source: fixture_source.clone(),
                wanted: None,
                pointer_on_timeline: false,
                first_hover_at: None,
                first_display_reported: false,
                cached_displays: Arc::default(),
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
        let (_, worker_grid) = session_grid(
            &main,
            &fixture_source,
            &Costs::load(None),
            number(&main, "duration").unwrap(),
        );
        let worker_step = worker_grid.step;
        assert_eq!(
            worker_step,
            std::env::var("PREVIEW_TEST_STEP")
                .unwrap_or("5".into())
                .parse::<f64>()
                .unwrap()
        );
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
        worker.start_with_source("fixture-one".into(), url.clone(), fixture_source.clone());
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
        worker.start_with_source("fixture-demand".into(), url.clone(), fixture_source);
        worker.hover("fixture-demand", true);
        let moved_to = if worker_step > 10.0 {
            3.5 * worker_step
        } else {
            22.5
        };
        *CHANGE_ON_SEEK.lock().unwrap() = Some((worker.shared.clone(), Some(moved_to)));
        worker.request("fixture-demand", Some(15.1));
        let (later, position) = loop {
            assert!(Instant::now() < deadline, "no requested thumbnail");
            if let Outbound::PreviewFrame {
                image,
                bucket: frame_bucket,
                position,
                ..
            } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
                && frame_bucket == bucket(15.1, worker_step).unwrap()
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
            if let Outbound::PreviewFrame {
                bucket: frame_bucket,
                ..
            } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
                && frame_bucket == bucket(moved_to, worker_step).unwrap()
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
                if 10.0 >= covers_from - worker_step / 2.0
                    && 10.0 <= covers_until + worker_step / 2.0
                {
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
                if 16.0 >= covers_from - worker_step / 2.0
                    && 16.0 <= covers_until + worker_step / 2.0
                {
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
        if std::env::var("PREVIEW_TEST_CONSERVATIVE_BACKGROUND").as_deref() == Ok("yes") {
            main.set_property("pause", "yes").unwrap();
            let paused_position = number(&main, "time-pos").unwrap();
            for (kind, id, counter) in [
                (
                    PreviewSourceKind::Http,
                    "fixture-http-background",
                    &HTTP_BACKGROUND_COUNT,
                ),
                (
                    PreviewSourceKind::Usenet,
                    "fixture-usenet-background",
                    &USENET_BACKGROUND_COUNT,
                ),
            ] {
                while receive.try_recv().is_ok() {}
                let before = counter.load(Ordering::Relaxed);
                let captures_before = BACKGROUND_CAPTURE_COUNT.load(Ordering::Relaxed);
                worker.start_with_source(
                    id.into(),
                    url.clone(),
                    PreviewSource {
                        kind,
                        mode: PreviewMode::Full,
                        colour: PreviewColour::Sdr,
                        ..PreviewSource::default()
                    },
                );
                let started = Instant::now();
                loop {
                    assert!(
                        started.elapsed() < Duration::from_secs(90),
                        "conservative background never admitted: {}",
                        preview_policy::kind_name(kind)
                    );
                    let _ = receive.recv_timeout(Duration::from_millis(250));
                    let count = counter.load(Ordering::Relaxed);
                    if count > before {
                        assert!(
                            started.elapsed() >= Duration::from_secs(59),
                            "background bypassed the full health window"
                        );
                        if BACKGROUND_CAPTURE_COUNT.load(Ordering::Relaxed) >= captures_before + 3 {
                            break;
                        }
                    }
                }
                // The scheduler must not change the main picture or seek its decoder.
                assert_eq!(main.get_property("pause", Kind::Flag), Some(true.into()));
                assert!((number(&main, "time-pos").unwrap() - paused_position).abs() < 0.1);
                worker.stop(id);
                std::thread::sleep(Duration::from_millis(250));
                let stopped = counter.load(Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(500));
                assert_eq!(
                    counter.load(Ordering::Relaxed),
                    stopped,
                    "stopped source continued speculative seeks"
                );
            }
        }
        if let Ok(flag) = std::env::var("PREVIEW_TEST_OUTAGE_FLAG")
            && !flag.is_empty()
        {
            // A server 503 proves recovery on a real failed open, not only a policy unit test.
            while receive.try_recv().is_ok() {}
            let failures = FAILED_COUNT.load(Ordering::Relaxed);
            std::fs::write(&flag, b"offline").unwrap();
            worker.start_with_source(
                "fixture-recovery".into(),
                url.clone(),
                PreviewSource {
                    kind: PreviewSourceKind::Debrid,
                    ..PreviewSource::default()
                },
            );
            worker.hover("fixture-recovery", true);
            worker.request("fixture-recovery", Some(30.1));
            let deadline = Instant::now() + Duration::from_secs(20);
            while FAILED_COUNT.load(Ordering::Relaxed) == failures {
                assert!(
                    Instant::now() < deadline,
                    "failed HTTP source never exercised recovery"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            std::fs::remove_file(&flag).unwrap();
            loop {
                assert!(
                    Instant::now() < deadline,
                    "preview did not recover after HTTP source returned"
                );
                if let Outbound::PreviewFrame {
                    session,
                    covers_from,
                    covers_until,
                    ..
                } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
                    && session == "fixture-recovery"
                    && 30.1 >= covers_from - worker_step / 2.0
                    && 30.1 <= covers_until + worker_step / 2.0
                {
                    break;
                }
            }
            worker.stop("fixture-recovery");
            while receive.try_recv().is_ok() {}
            let opens = OPEN_COUNT.load(Ordering::Relaxed);
            main.set_property("pause", "no").unwrap();
            // Exercise the explicit demand-only fallback independently of full-mode health tests.
            worker.start_with_source(
                "fixture-usenet".into(),
                url.clone(),
                PreviewSource {
                    kind: PreviewSourceKind::Usenet,
                    mode: PreviewMode::DemandOnly,
                    ..PreviewSource::default()
                },
            );
            std::thread::sleep(Duration::from_millis(500));
            assert_eq!(
                OPEN_COUNT.load(Ordering::Relaxed),
                opens,
                "Usenet opened before hover"
            );
            assert!(
                !receive
                    .try_iter()
                    .any(|m| matches!(m, Outbound::PreviewFrame { .. })),
                "Usenet generated an eager image"
            );
            worker.hover("fixture-usenet", true);
            worker.request("fixture-usenet", Some(30.1));
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                assert!(Instant::now() < deadline, "Usenet demand image missing");
                if let Outbound::PreviewFrame {
                    session,
                    covers_from,
                    covers_until,
                    ..
                } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
                    && session == "fixture-usenet"
                    && 30.1 >= covers_from - worker_step / 2.0
                    && 30.1 <= covers_until + worker_step / 2.0
                {
                    break;
                }
            }
            worker.hover("fixture-usenet", false);
            std::thread::sleep(Duration::from_millis(500));
            assert_eq!(
                USENET_BACKGROUND_COUNT.load(Ordering::Relaxed),
                0,
                "Usenet ran background seeks"
            );
            worker.stop("fixture-usenet");
            while receive.try_recv().is_ok() {}
            let opens = OPEN_COUNT.load(Ordering::Relaxed);
            worker.start_with_source(
                "fixture-http".into(),
                url.clone(),
                PreviewSource {
                    kind: PreviewSourceKind::Http,
                    mode: PreviewMode::Full,
                    ..PreviewSource::default()
                },
            );
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(
                OPEN_COUNT.load(Ordering::Relaxed),
                opens,
                "HTTP opened before hover or a complete health window"
            );
            worker.hover("fixture-http", true);
            worker.request("fixture-http", Some(30.1));
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                assert!(Instant::now() < deadline, "HTTP hover image missing");
                if let Outbound::PreviewFrame {
                    session,
                    covers_from,
                    covers_until,
                    ..
                } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
                    && session == "fixture-http"
                    && 30.1 >= covers_from - worker_step / 2.0
                    && 30.1 <= covers_until + worker_step / 2.0
                {
                    break;
                }
            }
            worker.hover("fixture-http", false);
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(
                HTTP_BACKGROUND_COUNT.load(Ordering::Relaxed),
                0,
                "HTTP bypassed the 60-second health gate"
            );
            worker.stop("fixture-http");
            main.set_property("pause", "yes").unwrap();
            while receive.try_recv().is_ok() {}
            let paused_position = number(&main, "time-pos").unwrap();
            worker.start_with_source(
                "fixture-paused".into(),
                url.clone(),
                PreviewSource {
                    kind: PreviewSourceKind::Debrid,
                    colour: PreviewColour::Sdr,
                    ..PreviewSource::default()
                },
            );
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "paused playback did not fill previews"
                );
                if let Outbound::PreviewFrame {
                    session, bucket, ..
                } = receive.recv_timeout(Duration::from_secs(10)).unwrap()
                    && session == "fixture-paused"
                    && bucket >= 2
                {
                    break;
                }
            }
            assert_eq!(main.get_property("pause", Kind::Flag), Some(true.into()));
            assert!((number(&main, "time-pos").unwrap() - paused_position).abs() < 0.1);
            worker.stop("fixture-paused");
        }
        while receive.try_recv().is_ok() {}
        worker.request("fixture-demand", Some(10.0));
        assert!(
            receive.recv_timeout(Duration::from_millis(500)).is_err(),
            "stopped session emitted frames"
        );
        drop(worker);
        // Even clearing the hover after a seek must finish and retain the image.
        let session = shared.0.lock().unwrap().session.clone().unwrap();
        let direct = open(&library, &main, &session, &shared, 1, false, false).unwrap();
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
            },
        );
        early.expect("moving the pointer cancelled an issued demand seek");
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
            },
        )
        .expect("newest demand failed after the preceding seek completed");
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
                WaitPolicy::new(WorkKind::Demand, true),
                None,
            )
            .unwrap();
            let baseline_session = shared.0.lock().unwrap().session.clone().unwrap();
            let baseline =
                open(&library, &main, &baseline_session, &shared, 1, false, false).unwrap();
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

    #[test]
    #[ignore = "requires libmpv and the HTTP server that refuses Range requests"]
    fn http_nonseekable_is_bounded_unavailable_without_images() {
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
                ],
            )
            .unwrap(),
        );
        main.set_property(
            "http-header-fields",
            "Authorization: Bearer preview-fixture",
        )
        .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = Previews::new(
            &library,
            main.clone(),
            Arc::new(move |m| {
                let _ = send.send(m);
            }),
        )
        .unwrap();
        worker.start_with_source(
            "fixture-http".into(),
            url.clone(),
            PreviewSource {
                kind: PreviewSourceKind::Http,
                ..PreviewSource::default()
            },
        );
        main.command(&["loadfile", &url]).unwrap();
        wait_frame(&main, &worker.shared, 1, Duration::from_secs(10)).unwrap();
        worker.hover("fixture-http", true);
        worker.request("fixture-http", Some(15.0));
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            assert!(
                Instant::now() < deadline,
                "HTTP nonseekable source did not become unavailable"
            );
            match receive.recv_timeout(Duration::from_secs(10)).unwrap() {
                Outbound::PreviewFrame { .. } => panic!("nonseekable HTTP generated an image"),
                Outbound::PreviewStatus { state, .. } if state == "unavailable" => break,
                _ => {}
            }
        }
        assert_eq!(main.get_property("pause", Kind::Flag), Some(true.into()));
        worker.stop("fixture-http");
    }

    #[test]
    #[ignore = "requires libmpv, the authenticated HTTP fixture and forced-unavailable environment"]
    fn forced_unavailable_keeps_main_playing_without_decoder_work() {
        assert_eq!(
            std::env::var("AIOSTREAMS_PREVIEW_TEST_UNAVAILABLE").as_deref(),
            Ok("on")
        );
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
                    ("pause", "no"),
                    ("idle", "yes"),
                ],
            )
            .unwrap(),
        );
        main.set_property(
            "http-header-fields",
            "Authorization: Bearer preview-fixture",
        )
        .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = Previews::new(
            &library,
            main.clone(),
            Arc::new(move |m| {
                let _ = send.send(m);
            }),
        )
        .unwrap();
        worker.start_with_source(
            "fixture-unavailable".into(),
            url.clone(),
            PreviewSource {
                kind: PreviewSourceKind::Debrid,
                colour: PreviewColour::Sdr,
                ..PreviewSource::default()
            },
        );
        main.command(&["loadfile", &url]).unwrap();
        wait_frame(&main, &worker.shared, 1, Duration::from_secs(10)).unwrap();
        let before = number(&main, "time-pos").unwrap();
        worker.hover("fixture-unavailable", true);
        worker.request("fixture-unavailable", Some(20.0));
        let mut terminal_seen = false;
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(Outbound::PreviewFrame { .. }) => panic!("forced unavailable emitted an image"),
                Ok(Outbound::PreviewStatus {
                    state,
                    reason,
                    terminal,
                    ..
                }) if state == "unavailable" && reason == "test-unavailable" && terminal => {
                    terminal_seen = true;
                }
                _ => {}
            }
        }
        assert!(terminal_seen, "forced terminal status was not emitted");
        assert_eq!(OPEN_COUNT.load(Ordering::Relaxed), 0);
        assert_eq!(main.get_property("pause", Kind::Flag), Some(false.into()));
        assert!(number(&main, "time-pos").unwrap() > before + 0.5);
        worker.stop("fixture-unavailable");
    }
}
