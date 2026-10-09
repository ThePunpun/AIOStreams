//! Pure preview policies. The worker supplies observed player health and successful
//! cold-seek costs; this module never opens connections or changes the main player.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bridge::{PreviewMode, PreviewSourceKind};

pub(crate) const MIN_GRID_SLOTS: u32 = 240;
pub(crate) const MAX_GRID_SLOTS: u32 = 600;
const FILL_TARGET_S: f64 = 480.0;
const EXPECTED_DUTY: f64 = 0.5;
const COST_SAMPLES: usize = 31;

pub(crate) fn kind_name(kind: PreviewSourceKind) -> &'static str {
    match kind {
        PreviewSourceKind::Debrid => "debrid",
        PreviewSourceKind::Usenet => "usenet",
        PreviewSourceKind::Http => "http",
        PreviewSourceKind::Unknown => "unknown",
    }
}

pub(crate) fn enforced_mode(
    kind: PreviewSourceKind,
    requested: PreviewMode,
    usenet_background: bool,
) -> PreviewMode {
    if requested == PreviewMode::Off || kind == PreviewSourceKind::Unknown {
        PreviewMode::Off
    } else if kind == PreviewSourceKind::Http
        || (kind == PreviewSourceKind::Usenet && !usenet_background)
    {
        PreviewMode::DemandOnly
    } else {
        requested
    }
}

pub(crate) fn usenet_background_enabled() -> bool {
    std::env::var("AIOSTREAMS_PREVIEW_USENET_BACKGROUND").as_deref() != Ok("off")
}

pub(crate) fn seekable_for_preview(seekable: bool, partially_seekable: bool) -> bool {
    seekable && !partially_seekable
}

pub(crate) struct Grid {
    pub slots: u32,
    pub step: f64,
    pub cost_s: f64,
    pub learned: bool,
}

pub(crate) fn grid_for(duration: f64, cost_s: f64, learned: bool) -> Grid {
    let cost_s = if cost_s.is_finite() && cost_s > 0.0 {
        cost_s
    } else {
        0.9
    };
    let slots = (FILL_TARGET_S * EXPECTED_DUTY / cost_s)
        .round()
        .clamp(f64::from(MIN_GRID_SLOTS), f64::from(MAX_GRID_SLOTS)) as u32;
    let step = if duration.is_finite() && duration > 0.0 {
        ((duration.min(86400.0) * 1000.0 / f64::from(slots)).ceil() / 1000.0).max(5.0)
    } else {
        5.0
    };
    Grid {
        slots,
        step,
        cost_s,
        learned,
    }
}

pub(crate) fn default_cost(kind: PreviewSourceKind, range: &str, wide: bool) -> f64 {
    match (kind, range == "sdr" && !wide) {
        (PreviewSourceKind::Usenet, true) => 0.3,
        (PreviewSourceKind::Usenet, false) => 0.75,
        (_, true) => 0.35,
        (_, false) => 0.9,
    }
}

pub(crate) fn cost_class(kind: PreviewSourceKind, range: &str, wide: bool) -> String {
    format!(
        "{}:{range}:{}",
        kind_name(kind),
        if wide { "wide" } else { "hd" }
    )
}

fn valid_class(class: &str) -> bool {
    let fields: Vec<_> = class.split(':').collect();
    fields.len() == 3
        && matches!(fields[0], "debrid" | "usenet" | "http")
        && matches!(fields[1], "sdr" | "hdr" | "dv")
        && matches!(fields[2], "hd" | "wide")
}

#[derive(Default, Serialize, Deserialize)]
struct CostFile {
    version: u32,
    samples: BTreeMap<String, VecDeque<f64>>,
}

pub(crate) struct Costs {
    file: CostFile,
    path: Option<PathBuf>,
    dirty: bool,
}

impl Costs {
    pub fn load(directory: Option<&Path>) -> Self {
        let path = directory.map(|dir| dir.join("preview-seek-costs.json"));
        let file = path
            .as_ref()
            .and_then(|path| {
                std::fs::metadata(path)
                    .ok()
                    .filter(|m| m.len() <= 65536)
                    .map(|_| path)
            })
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<CostFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .unwrap_or_else(|| CostFile {
                version: 1,
                ..CostFile::default()
            });
        let mut costs = Self {
            file,
            path,
            dirty: false,
        };
        costs.file.samples.retain(|class, samples| {
            samples.retain(|s| s.is_finite() && (0.05..=15.0).contains(s));
            while samples.len() > COST_SAMPLES {
                samples.pop_front();
            }
            valid_class(class) && !samples.is_empty()
        });
        costs
    }

    pub fn grid(&self, duration: f64, class: &str, default: f64) -> Grid {
        let learned = self
            .file
            .samples
            .get(class)
            .filter(|samples| samples.len() >= 5);
        let cost = learned.map(|samples| {
            let mut sorted: Vec<_> = samples.iter().copied().collect();
            sorted.sort_unstable_by(f64::total_cmp);
            sorted[sorted.len() / 2]
        });
        grid_for(duration, cost.unwrap_or(default), cost.is_some())
    }

    pub fn observe(&mut self, class: &str, cost_s: f64) {
        // Ignore opening frames, cached decoder seeks, failures and absurd costs.
        if !valid_class(class) || !cost_s.is_finite() || !(0.05..=15.0).contains(&cost_s) {
            return;
        }
        let samples = self.file.samples.entry(class.into()).or_default();
        samples.push_back(cost_s);
        while samples.len() > COST_SAMPLES {
            samples.pop_front();
        }
        self.dirty = true;
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = &self.path else {
            self.dirty = false;
            return;
        };
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("json.tmp");
            let bytes = serde_json::to_vec(&self.file)?;
            std::fs::write(&temporary, bytes)?;
            // Windows rename cannot replace an existing file. A failed replacement
            // affects only learning: no URLs, credentials or thumbnails live here.
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            std::fs::rename(temporary, path)
        })();
        if result.is_ok() {
            self.dirty = false;
        } else {
            log::info!(target: "seek_preview", "grid_cost_persist=failed");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BufferingCause {
    Seek,
    Underrun,
    Startup,
}
impl BufferingCause {
    pub fn name(self) -> &'static str {
        match self {
            Self::Seek => "seek",
            Self::Underrun => "underrun",
            Self::Startup => "startup",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Health {
    pub at: f64,
    pub buffered: Option<f64>,
    pub position: f64,
    pub speed: f64,
    pub paused: bool,
    pub seeking: bool,
    pub user_seek: bool,
    pub buffering: bool,
    pub underrun: bool,
    pub reader_idle: bool,
    pub pointer: bool,
    pub current: bool,
}

pub(crate) struct Duty {
    pub value: f64,
    pub level: f64,
    pub reason: &'static str,
    pub cause: Option<BufferingCause>,
    pub at_buffering: Option<f64>,
    pub throttled: bool,
}

pub(crate) struct Controller {
    samples: VecDeque<(f64, f64)>,
    previous: Option<Health>,
    level: usize,
    ceiling: usize,
    stable_since: f64,
    baseline: Option<f64>,
    seek_until: f64,
    throttle_until: f64,
    last_buffering: Option<f64>,
    comfortable: bool,
    buffering_cause: Option<BufferingCause>,
    last_duty: f64,
    usenet_attempts: VecDeque<f64>,
}

impl Controller {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            previous: None,
            level: 0,
            ceiling: 2,
            stable_since: 0.0,
            baseline: None,
            seek_until: 0.0,
            throttle_until: 0.0,
            last_buffering: None,
            comfortable: false,
            buffering_cause: None,
            last_duty: 0.0,
            usenet_attempts: VecDeque::new(),
        }
    }

    pub fn minimum(&self, now: f64, window: f64) -> Option<f64> {
        self.samples
            .iter()
            .filter(|(at, _)| *at >= now - window)
            .map(|(_, seconds)| *seconds)
            .reduce(f64::min)
    }

    fn complete_window(&self, now: f64, window: f64) -> bool {
        self.samples
            .front()
            .is_some_and(|(at, _)| now - at >= window)
    }

    pub fn usenet_attempt(&mut self, now: f64) {
        self.usenet_attempts.push_back(now);
    }

    pub fn update(&mut self, h: Health, kind: PreviewSourceKind, full: bool) -> Duty {
        const LEVELS: [f64; 3] = [0.5, 0.65, 0.8];
        let buffered = h.buffered.filter(|s| s.is_finite() && *s >= 0.0);
        let mut dropped = false;
        let discontinuity = self
            .previous
            .filter(|p| p.current && h.current)
            .is_some_and(|p| {
                let expected = if p.paused {
                    0.0
                } else {
                    (h.at - p.at) * p.speed
                };
                let advance = h.position - p.position;
                // A stalled clock is evidence of buffering, not a forward seek.
                advance < -2.0 || advance > expected + 2.0
            });
        let seek = h.seeking || discontinuity;
        if seek {
            self.seek_until = h.at + 5.0;
        }
        let new_buffering = h.current && h.buffering && self.previous.is_none_or(|p| !p.buffering);
        let new_underrun = h.current && h.underrun && self.previous.is_none_or(|p| !p.underrun);
        let cause = (new_buffering || new_underrun).then_some({
            if seek
                || h.user_seek
                || h.at < self.seek_until
                || h.buffering && self.buffering_cause == Some(BufferingCause::Seek)
            {
                BufferingCause::Seek
            } else if !self.comfortable {
                BufferingCause::Startup
            } else {
                BufferingCause::Underrun
            }
        });
        let at_buffering = cause.map(|_| self.last_duty);
        if new_buffering {
            self.buffering_cause = cause;
        }
        if h.current && h.buffering {
            self.last_buffering = Some(h.at);
        }
        if cause == Some(BufferingCause::Underrun) {
            if self.last_duty > 0.5 && self.level > 0 {
                self.ceiling = self.ceiling.min(self.level - 1);
            }
            self.level = self.level.saturating_sub(1).min(self.ceiling);
            self.throttle_until = h.at + 60.0;
            self.samples.clear();
            self.baseline = None;
            self.stable_since = h.at;
        }
        // Keep the post-underrun rest measured from recovery, not from stall start.
        if h.buffering && self.buffering_cause == Some(BufferingCause::Underrun) {
            self.throttle_until = self.throttle_until.max(h.at + 60.0);
        }
        if !h.buffering {
            self.buffering_cause = None;
        }
        if seek
            || h.buffering
            || !h.current
            || buffered.is_none()
            || self.previous.is_some_and(|p| h.at - p.at > 10.0)
        {
            self.samples.clear();
            self.baseline = None;
            self.stable_since = h.at;
        } else if let Some(seconds) = buffered {
            self.samples.push_back((h.at, seconds));
            while self
                .samples
                .get(1)
                .is_some_and(|(at, _)| *at <= h.at - 60.0)
            {
                self.samples.pop_front();
            }
            let minimum = self.minimum(h.at, 30.0).unwrap_or(seconds);
            let previous_min = self.baseline.get_or_insert(minimum);
            if h.at >= self.seek_until && minimum < *previous_min * 0.75 {
                self.level = self.level.saturating_sub(1);
                dropped = true;
                *previous_min = minimum;
                self.stable_since = h.at;
            } else if h.at - self.stable_since >= 30.0 && h.at >= self.throttle_until {
                if minimum >= 10.0 && minimum >= *previous_min * 0.8 && !h.underrun {
                    self.level = (self.level + 1).min(self.ceiling);
                }
                *previous_min = minimum;
                self.stable_since = h.at;
            }
            self.comfortable |= seconds >= 10.0;
        }
        while self
            .usenet_attempts
            .front()
            .is_some_and(|at| h.at - *at >= 60.0)
        {
            self.usenet_attempts.pop_front();
        }
        let throttled = h.at < self.throttle_until;
        let reason = if !full {
            "demand-only"
        } else if !h.current {
            "source-inactive"
        } else if h.pointer {
            "pointer"
        } else if seek || h.at < self.seek_until && h.buffering {
            "seek"
        } else if h.buffering || h.underrun {
            "underrun"
        } else if buffered.is_none_or(|s| s < 10.0) {
            "buffer-low-or-unknown"
        } else if kind == PreviewSourceKind::Usenet
            && (!self.complete_window(h.at, 60.0)
                || self.minimum(h.at, 60.0).is_none_or(|s| s < 30.0)
                || !h.reader_idle
                || self.last_buffering.is_some_and(|at| h.at - at < 300.0))
        {
            "usenet-health"
        } else if kind == PreviewSourceKind::Usenet && self.usenet_attempts.len() >= 30 {
            "usenet-rate-limit"
        } else if throttled {
            "underrun-backoff"
        } else if dropped {
            "drop"
        } else if h.paused {
            "paused"
        } else {
            "stable"
        };
        let allowed = matches!(reason, "stable" | "paused" | "drop" | "underrun-backoff");
        let value = if !allowed {
            0.0
        } else if kind == PreviewSourceKind::Usenet || throttled {
            0.25
        } else {
            LEVELS[self.level]
        };
        self.previous = Some(h);
        self.last_duty = value;
        Duty {
            value,
            level: LEVELS[self.level],
            reason,
            cause,
            at_buffering,
            throttled,
        }
    }
}

pub(crate) fn open_timeout_s(
    usenet: bool,
    elapsed: f64,
    input_waiting: bool,
    progress: bool,
) -> bool {
    elapsed
        >= if usenet && input_waiting && progress {
            15.0
        } else {
            5.0
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn health(at: f64, buffered: f64) -> Health {
        Health {
            at,
            buffered: Some(buffered),
            position: at,
            speed: 1.0,
            paused: false,
            seeking: false,
            user_seek: false,
            buffering: false,
            underrun: false,
            reader_idle: true,
            pointer: false,
            current: true,
        }
    }
    #[test]
    fn four_grid_cases_and_invalid_costs_are_bounded() {
        let lotr = grid_for(13692.0, 0.78, true);
        assert_eq!(lotr.slots, 308);
        assert_eq!(lotr.step, 44.455);
        assert_eq!(grid_for(2018.0, 1.04, true).step, 8.409);
        assert_eq!(grid_for(1420.0, 0.33, true).step, 5.0);
        assert_eq!(grid_for(8700.0, 0.75, false).step, 27.188);
        for cost in [0.0, f64::NAN, f64::INFINITY, 0.001, 100.0] {
            let grid = grid_for(86400.0, cost, false);
            assert!((240..=600).contains(&grid.slots));
            assert!(grid.step <= 360.0);
        }
    }
    #[test]
    fn learned_median_is_bounded_and_applies_only_to_the_next_grid() {
        let mut costs = Costs::load(None);
        let class = cost_class(PreviewSourceKind::Debrid, "hdr", true);
        let original = costs.grid(13692.0, &class, 0.9);
        for cost in [0.7, 0.77, 0.78, 0.8, 8.0] {
            costs.observe(&class, cost);
        }
        let learned = costs.grid(13692.0, &class, 0.9);
        assert!(!original.learned);
        assert_eq!(original.slots, 267);
        assert!(learned.learned);
        assert_eq!(learned.slots, 308);
        for _ in 0..100 {
            costs.observe(&class, 0.75);
        }
        costs.observe("not:a:class", 0.75);
        costs.observe(&class, f64::NAN);
        assert_eq!(costs.file.samples[&class].len(), COST_SAMPLES);
        assert_eq!(costs.file.samples.len(), 1);
    }
    #[test]
    fn stable_small_buffers_step_up_without_idle_and_paused_fill_is_allowed() {
        let mut c = Controller::new();
        for at in 0..=90 {
            let mut h = health(f64::from(at), 15.0);
            h.reader_idle = false;
            let d = c.update(h, PreviewSourceKind::Debrid, true);
            assert_eq!(
                d.value,
                if at < 30 {
                    0.5
                } else if at < 60 {
                    0.65
                } else {
                    0.8
                }
            );
        }
        let mut h = health(91.0, 15.0);
        h.paused = true;
        let d = c.update(h, PreviewSourceKind::Debrid, true);
        assert_eq!(d.value, 0.8);
        assert_eq!(d.reason, "paused");
        h.at = 92.0;
        h.position = 91.0;
        h.buffered = Some(9.0);
        assert_eq!(c.update(h, PreviewSourceKind::Debrid, true).value, 0.0);
    }
    #[test]
    fn a_large_buffer_drop_steps_down_and_logs_the_transition() {
        let mut c = Controller::new();
        for at in 0..=60 {
            c.update(health(f64::from(at), 50.0), PreviewSourceKind::Debrid, true);
        }
        let d = c.update(health(61.0, 35.0), PreviewSourceKind::Debrid, true);
        assert_eq!(d.value, 0.65);
        assert_eq!(d.reason, "drop");
        assert!(!d.throttled);
        assert_eq!(d.cause, None);
    }

    #[test]
    fn deliberate_seeks_do_not_trigger_underrun_cooldown() {
        let mut c = Controller::new();
        c.update(health(0.0, 50.0), PreviewSourceKind::Debrid, true);
        let mut h = health(1.0, 0.0);
        h.position = 400.0;
        h.buffering = true;
        let d = c.update(h, PreviewSourceKind::Debrid, true);
        assert_eq!(d.cause, Some(BufferingCause::Seek));
        assert!(!d.throttled);
        h.at = 2.0;
        h.buffering = false;
        h.buffered = Some(50.0);
        assert_eq!(c.update(h, PreviewSourceKind::Debrid, true).value, 0.5);
    }
    #[test]
    fn unexpected_buffering_steps_down_and_caps_the_session() {
        let mut c = Controller::new();
        for at in 0..=60 {
            c.update(health(f64::from(at), 50.0), PreviewSourceKind::Debrid, true);
        }
        let mut h = health(61.0, 0.0);
        h.buffering = true;
        let d = c.update(h, PreviewSourceKind::Debrid, true);
        assert_eq!(d.cause, Some(BufferingCause::Underrun));
        assert_eq!(d.at_buffering, Some(0.8));
        assert_eq!(d.level, 0.65);
        assert_eq!(d.value, 0.0);
        for at in 62..=200 {
            let d = c.update(health(f64::from(at), 50.0), PreviewSourceKind::Debrid, true);
            assert!(d.level <= 0.65);
            if at < 121 {
                assert_eq!(d.value, 0.25);
            }
        }
    }
    #[test]
    fn usenet_requires_a_complete_healthy_window_and_limits_attempts() {
        let mut c = Controller::new();
        for at in 0..=60 {
            let d = c.update(
                health(f64::from(at), 120.0),
                PreviewSourceKind::Usenet,
                true,
            );
            assert_eq!(d.value, if at < 60 { 0.0 } else { 0.25 });
        }
        for _ in 0..30 {
            c.usenet_attempt(60.0);
        }
        assert_eq!(
            c.update(health(61.0, 120.0), PreviewSourceKind::Usenet, true)
                .reason,
            "usenet-rate-limit"
        );
        let mut h = health(62.0, 19.0);
        assert_eq!(c.update(h, PreviewSourceKind::Usenet, true).value, 0.0);
        h.at = 63.0;
        h.buffered = Some(120.0);
        h.reader_idle = false;
        assert_eq!(c.update(h, PreviewSourceKind::Usenet, true).value, 0.0);
    }
    #[test]
    fn source_and_timeout_gates_are_native_and_bounded() {
        assert_eq!(
            enforced_mode(PreviewSourceKind::Http, PreviewMode::Full, true),
            PreviewMode::DemandOnly
        );
        assert_eq!(
            enforced_mode(PreviewSourceKind::Usenet, PreviewMode::Full, false),
            PreviewMode::DemandOnly
        );
        assert_eq!(
            enforced_mode(PreviewSourceKind::Unknown, PreviewMode::Full, true),
            PreviewMode::Off
        );
        assert!(!seekable_for_preview(true, true));
        assert!(seekable_for_preview(true, false));
        assert!(open_timeout_s(false, 5.0, true, true));
        assert!(!open_timeout_s(true, 5.0, true, true));
        assert!(open_timeout_s(true, 15.0, true, true));
        assert!(open_timeout_s(true, 5.0, true, false));
    }

    #[test]
    fn delayed_seek_buffering_never_becomes_an_underrun_throttle() {
        let mut c = Controller::new();
        c.update(health(0.0, 50.0), PreviewSourceKind::Debrid, true);
        let mut h = health(1.0, 0.0);
        h.user_seek = true;
        h.buffering = true;
        h.position = 1.0;
        assert_eq!(
            c.update(h, PreviewSourceKind::Debrid, true).cause,
            Some(BufferingCause::Seek)
        );
        for at in 2..=25 {
            h.at = f64::from(at);
            h.user_seek = false;
            h.underrun = at == 10;
            assert!(!c.update(h, PreviewSourceKind::Debrid, true).throttled);
        }
        h.at = 26.0;
        h.buffering = false;
        h.buffered = Some(50.0);
        assert_eq!(c.update(h, PreviewSourceKind::Debrid, true).value, 0.5);
    }

    #[test]
    fn a_stalled_clock_is_not_evidence_of_a_seek() {
        let mut c = Controller::new();
        c.update(health(0.0, 50.0), PreviewSourceKind::Debrid, true);
        let mut h = health(8.0, 0.0);
        h.position = 0.0;
        h.buffering = true;
        assert_eq!(
            c.update(h, PreviewSourceKind::Debrid, true).cause,
            Some(BufferingCause::Underrun)
        );
    }

    #[test]
    fn lost_health_history_and_recent_buffering_block_usenet() {
        let mut c = Controller::new();
        for at in 0..=60 {
            c.update(
                health(f64::from(at), 120.0),
                PreviewSourceKind::Usenet,
                true,
            );
        }
        let mut h = health(61.0, 0.0);
        h.position = 400.0;
        h.buffering = true;
        c.update(h, PreviewSourceKind::Usenet, true);
        for at in 62..=400 {
            let mut h = health(f64::from(at), 120.0);
            h.position += 339.0;
            let d = c.update(h, PreviewSourceKind::Usenet, true);
            assert_eq!(d.value, if at < 361 { 0.0 } else { 0.25 });
        }
        assert_eq!(
            c.update(health(411.0, 120.0), PreviewSourceKind::Usenet, true)
                .value,
            0.0
        );
    }

    #[test]
    fn learned_data_roundtrips_and_corruption_falls_back_without_disabling_previews() {
        let dir =
            std::env::temp_dir().join(format!("aio-preview-cost-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let class = "debrid:hdr:wide";
        let mut costs = Costs::load(Some(&dir));
        for _ in 0..5 {
            costs.observe(class, 0.78);
        }
        costs.save();
        assert!(Costs::load(Some(&dir)).grid(13692.0, class, 0.9).learned);
        std::fs::write(dir.join("preview-seek-costs.json"), b"invalid").unwrap();
        assert!(!Costs::load(Some(&dir)).grid(13692.0, class, 0.9).learned);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
