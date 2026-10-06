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

const STEP: f64 = 2.0;
const MAX_IMAGES: usize = 96;
const MAX_BACKGROUND: usize = 16;
const MAX_READ_BYTES: f64 = 128.0 * 1024.0 * 1024.0;

#[derive(Clone)]
struct Session {
    id: String,
    url: String,
    wanted: Option<u32>,
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
            s.wanted = seconds.and_then(bucket);
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

fn bucket(seconds: f64) -> Option<u32> {
    (seconds.is_finite() && (0.0..=24.0 * 3600.0).contains(&seconds))
        .then(|| (seconds / STEP).floor() as u32)
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
    main.get_property("path", Kind::String)
        .and_then(|v| v.as_str().map(str::to_owned))
        .as_deref()
        == Some(url)
        && main.get_property("idle-active", Kind::Flag) == Some(false.into())
        && main.get_property("paused-for-cache", Kind::Flag) != Some(true.into())
        && main.get_property("seeking", Kind::Flag) != Some(true.into())
        && number(main, "time-pos").is_some()
        && number(main, "demuxer-cache-duration").is_none_or(|v| v == 0.0 || v >= 1.0)
}

#[derive(Clone, Copy)]
enum Work {
    Opening,
    Hover(u32),
    Background,
}

fn wait_frame(
    mpv: &Mpv,
    shared: &Shared,
    generation: u64,
    work: Work,
    timeout: Duration,
    playback: Option<(&Mpv, &str)>,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !active(shared, generation) {
            return Err("cancelled");
        }
        if let Some((main, url)) = playback
            && !healthy(main, url)
        {
            return Err("playback-busy");
        }
        if !matches!(work, Work::Opening) {
            let wanted = shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .and_then(|s| s.wanted);
            if match work {
                Work::Hover(target) => wanted != Some(target),
                Work::Background => wanted.is_some(),
                Work::Opening => false,
            } {
                return Err("superseded");
            }
        }
        match mpv.wait_event(0.05, |_| false) {
            Some(Event::PlaybackRestart) => return Ok(()),
            Some(
                Event::EndFile {
                    reason: "error", ..
                }
                | Event::Shutdown,
            ) => return Err("decode-error"),
            _ => {}
        }
    }
    Err("timeout")
}

#[derive(Clone)]
struct Image {
    bucket: u32,
    position: f64,
    data: String,
}

#[derive(Default)]
struct Stats {
    generated: usize,
    background: usize,
    hits: usize,
    errors: usize,
    superseded: usize,
    demux_bytes: Option<f64>,
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
) -> Result<Decoder, &'static str> {
    let mpv = Mpv::new(
        library,
        &[
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
        ],
    )
    .map_err(|_| "decoder-init")?;
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
    wait_frame(
        &decoder.mpv,
        shared,
        generation,
        Work::Opening,
        Duration::from_secs(12),
        Some((main, &session.url)),
    )?;
    if decoder.mpv.get_property("seekable", Kind::Flag) != Some(true.into()) {
        return Err("not-seekable");
    }
    let gamma = decoder
        .mpv
        .get_property("video-dec-params", Kind::Json)
        .and_then(|v| v.get("gamma").and_then(|v| v.as_str()).map(str::to_owned));
    if matches!(gamma.as_deref(), Some("pq" | "hlg")) {
        decoder.mpv.set_property("vf", "lavfi=[zscale=transfer=linear,format=gbrpf32le,tonemap=hable,zscale=transfer=bt709:primaries=bt709:matrix=bt709,scale=320:-2,format=yuv420p]").map_err(|_| "hdr-filter")?;
    }
    log::info!(target: "seek_preview", "session={} opened_ms={} hdr={}", session.id, start.elapsed().as_millis(), matches!(gamma.as_deref(), Some("pq" | "hlg")));
    Ok(decoder)
}

fn capture(
    decoder: &Decoder,
    main: &Mpv,
    session: &Session,
    shared: &Shared,
    generation: u64,
    target: u32,
    demand: bool,
) -> Result<Image, &'static str> {
    // Drain old restart events so an earlier seek cannot complete this request.
    while decoder.mpv.wait_event(0.0, |_| false).is_some() {}
    let started = Instant::now();
    decoder
        .mpv
        .command(&[
            "seek",
            &(f64::from(target) * STEP).to_string(),
            "absolute",
            "keyframes",
        ])
        .map_err(|_| "seek-command")?;
    wait_frame(
        &decoder.mpv,
        shared,
        generation,
        if demand {
            Work::Hover(target)
        } else {
            Work::Background
        },
        Duration::from_secs(8),
        Some((main, &session.url)),
    )?;
    let seek_ms = started.elapsed().as_millis();
    let position = number(&decoder.mpv, "time-pos").ok_or("no-position")?;
    let path = decoder.dir.join("frame.jpg");
    decoder
        .mpv
        .command(&["screenshot-to-file", &path.to_string_lossy(), "video"])
        .map_err(|_| "screenshot")?;
    let bytes = std::fs::read(&path).map_err(|_| "read-image")?;
    let _ = std::fs::remove_file(&path);
    if bytes.len() > 512 * 1024 || !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("invalid-image");
    }
    log::info!(target: "seek_preview", "session={} bucket={} sampled_s={position:.3} demand={} seek_ms={seek_ms} capture_ms={} image_bytes={}", session.id, target, demand, started.elapsed().as_millis().saturating_sub(seek_ms), bytes.len());
    Ok(Image {
        bucket: target,
        position,
        data: format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes)),
    })
}

fn bytes_read(decoder: &Decoder) -> Option<f64> {
    decoder
        .mpv
        .get_property("demuxer-cache-state", Kind::Json)?
        .get("reader-total-bytes")?
        .as_f64()
}

fn close_decoder(decoder: &mut Option<Decoder>, stats: &mut Stats) {
    if let Some(d) = decoder.take()
        && let Some(bytes) = bytes_read(&d)
    {
        stats.demux_bytes = Some(stats.demux_bytes.unwrap_or(0.0) + bytes);
    }
}

fn finish_request(shared: &Shared, generation: u64, target: u32) {
    let mut state = shared.0.lock().unwrap();
    if state.generation == generation
        && let Some(s) = state.session.as_mut()
        && s.wanted == Some(target)
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
        let mut backoff = Duration::from_millis(500);
        let mut failed = false;
        let mut status = "";
        log::info!(target: "seek_preview", "session={} start cached_debrid=true max_background={MAX_BACKGROUND}", session.id);
        while active(shared, generation) {
            let healthy = healthy(main, &session.url);
            let next_status = if failed {
                "unavailable"
            } else if !healthy {
                "waiting"
            } else {
                "ready"
            };
            if status != next_status {
                status = next_status;
                emit(Outbound::PreviewStatus {
                    session: session.id.clone(),
                    state: status.into(),
                });
                log::info!(target: "seek_preview", "session={} state={status}", session.id);
            }
            if !healthy || failed {
                healthy_since = None;
                if !healthy {
                    close_decoder(&mut decoder, &mut stats);
                }
                std::thread::park_timeout(Duration::from_millis(150));
                continue;
            }
            if healthy_since.get_or_insert_with(Instant::now).elapsed() < Duration::from_secs(2) {
                std::thread::park_timeout(Duration::from_millis(100));
                continue;
            }
            if decoder.is_none() {
                match open(library, main, &session, shared, generation) {
                    Ok(d) => decoder = Some(d),
                    Err("cancelled") => break,
                    Err("playback-busy") => continue,
                    Err(reason) => {
                        log::info!(target: "seek_preview", "session={} open_failed={reason}", session.id);
                        stats.errors += 1;
                        failed = true;
                        continue;
                    }
                }
            }
            let d = decoder.as_ref().unwrap();
            if bytes_read(d).is_some_and(|v| v + stats.demux_bytes.unwrap_or(0.0) > MAX_READ_BYTES)
            {
                log::info!(target: "seek_preview", "session={} budget_exhausted=demux_bytes", session.id);
                failed = true;
                close_decoder(&mut decoder, &mut stats);
                continue;
            }
            let wanted = shared
                .0
                .lock()
                .unwrap()
                .session
                .as_ref()
                .and_then(|s| s.wanted);
            let demand = wanted.is_some();
            let target = wanted.or_else(|| {
                if stats.background >= MAX_BACKGROUND || Instant::now() < background_at {
                    return None;
                }
                let duration = number(main, "duration")?;
                let position = number(main, "time-pos").unwrap_or(0.0);
                // Nearby first, then coarse coverage: this never scans the entire file.
                let seconds = match stats.background {
                    0 => position,
                    1 => position + 15.0,
                    2 => (position - 15.0).max(0.0),
                    n => duration * (n - 2) as f64 / 14.0,
                };
                bucket(seconds.min((duration - 1.0).max(0.0)))
            });
            let Some(target) = target else {
                let state = shared.0.lock().unwrap();
                let _ = shared
                    .1
                    .wait_timeout(state, Duration::from_millis(150))
                    .unwrap();
                continue;
            };
            if let Some(i) = cache.iter().find(|i| i.bucket == target) {
                if demand {
                    stats.hits += 1;
                    deliver(emit, &session, i, true, 0);
                    finish_request(shared, generation, target);
                } else {
                    stats.background += 1;
                }
                continue;
            }
            let started = Instant::now();
            match capture(d, main, &session, shared, generation, target, demand) {
                Ok(image) => {
                    if !active(shared, generation) {
                        break;
                    }
                    stats.generated += 1;
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
                }
                Err("cancelled") => break,
                Err("superseded") => {
                    stats.superseded += 1;
                }
                Err("playback-busy") => {
                    close_decoder(&mut decoder, &mut stats);
                    healthy_since = None;
                    continue;
                }
                Err(reason) => {
                    stats.errors += 1;
                    log::info!(target: "seek_preview", "session={} bucket={target} failed={reason}", session.id);
                    if stats.errors >= 3 {
                        failed = true;
                        close_decoder(&mut decoder, &mut stats);
                    }
                }
            }
            if demand {
                finish_request(shared, generation, target);
            } else {
                stats.background += 1;
            }
            background_at = Instant::now() + backoff;
            if stats.generated >= 256 {
                failed = true;
                close_decoder(&mut decoder, &mut stats);
            }
        }
        close_decoder(&mut decoder, &mut stats);
        log::info!(target: "seek_preview", "session={} summary generated={} background={} native_hits={} errors={} superseded={} demux_bytes={:?}", session.id, stats.generated, stats.background, stats.hits, stats.errors, stats.superseded, stats.demux_bytes);
    }
}

fn deliver(emit: &Emit, session: &Session, image: &Image, cached: bool, elapsed_ms: u64) {
    emit(Outbound::PreviewFrame {
        session: session.id.clone(),
        bucket: image.bucket,
        position: image.position,
        image: image.data.clone(),
        cached,
        elapsed_ms,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_positions_and_groups_nearby_requests() {
        assert_eq!(bucket(f64::NAN), None);
        assert_eq!(bucket(f64::INFINITY), None);
        assert_eq!(bucket(-1.0), None);
        assert_eq!(bucket(1e20), None);
        assert_eq!(bucket(3.9), Some(1));
        assert_eq!(bucket(4.0), Some(2));
    }

    #[test]
    #[ignore = "requires libmpv and the authenticated HTTP fixture"]
    fn remote_decoder_and_session_cleanup() {
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
        wait_frame(
            &main,
            &shared,
            1,
            Work::Opening,
            Duration::from_secs(10),
            None,
        )
        .unwrap();
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
        worker.request("fixture-one", Some(6.1));
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
        assert!(
            (position - 6.0).abs() < 1.0,
            "wrong sampled position: {position}"
        );
        assert_ne!(first, later, "distant positions have identical images");
        if let Ok(dir) = std::env::var("PREVIEW_TEST_OUTPUT") {
            for (name, data) in [("first.jpg", &first), ("later.jpg", &later)] {
                let bytes = STANDARD
                    .decode(data.strip_prefix("data:image/jpeg;base64,").unwrap())
                    .unwrap();
                std::fs::write(Path::new(&dir).join(name), bytes).unwrap();
            }
        }
        worker.request("fixture-one", Some(8.0));
        worker.request("fixture-one", Some(4.0));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "latest requested position lost");
            if let Outbound::PreviewFrame { bucket: 2, .. } =
                receive.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break;
            }
        }
        worker.stop("fixture-one");
        while receive.try_recv().is_ok() {}
        worker.request("fixture-one", Some(10.0));
        assert!(
            receive.recv_timeout(Duration::from_millis(500)).is_err(),
            "stopped session emitted frames"
        );
        drop(worker);
    }
}
