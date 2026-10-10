//! Pure preview policies. The worker supplies observed player health and successful
//! cold-seek costs; this module never opens connections or changes the main player.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bridge::{PreviewMode, PreviewSourceKind};

pub(crate) const MIN_GRID_SLOTS: u32 = 240;
pub(crate) const MAX_GRID_SLOTS: u32 = 600;
const FILL_TARGET_S: f64 = 480.0;
const COST_SAMPLES: usize = 31;
const MIN_COST_SAMPLES: usize = 10;
pub(crate) const STABLE_WINDOW_S: f64 = 30.0;
pub(crate) const PROMOTION_MIN_RATIO: f64 = 0.8;
pub(crate) const DROP_MIN_RATIO: f64 = 0.75;
pub(crate) const UNDERRUN_BACKOFF_S: f64 = 60.0;
pub(crate) const CONSERVATIVE_WINDOW_S: f64 = 60.0;
pub(crate) const CONSERVATIVE_START_BUFFER_S: f64 = 30.0;
pub(crate) const CONSERVATIVE_ABORT_BUFFER_S: f64 = 20.0;
pub(crate) const CONSERVATIVE_COOLDOWN_S: f64 = 300.0;
pub(crate) const MIN_BACKGROUND_BUFFER_S: f64 = 10.0;
const MIN_CONSERVATIVE_DUTY: f64 = 0.25;
pub(crate) const DEFAULT_CONSERVATIVE_DUTY: f64 = 0.5;
const MAX_CONSERVATIVE_DUTY: f64 = 0.5;
const DEBRID_LEVELS: [f64; 3] = [0.5, 0.65, 0.8];
const CONSERVATIVE_LEVELS: [f64; 3] = [0.25, 0.35, 0.5];

pub(crate) fn configured_ceiling(kind: PreviewSourceKind) -> f64 {
    let name = match kind {
        PreviewSourceKind::Usenet => "AIOSTREAMS_PREVIEW_USENET_MAX_DUTY",
        PreviewSourceKind::Http => "AIOSTREAMS_PREVIEW_HTTP_MAX_DUTY",
        _ => return DEBRID_LEVELS[2],
    };
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && (MIN_CONSERVATIVE_DUTY..=MAX_CONSERVATIVE_DUTY).contains(v))
        .unwrap_or(DEFAULT_CONSERVATIVE_DUTY)
}

pub(crate) fn seek_cap(ceiling: f64) -> usize {
    if ceiling > MIN_CONSERVATIVE_DUTY {
        60
    } else {
        30
    }
}

pub(crate) fn log_constants() {
    log::info!(target: "seek_preview", "policy_fill_target_s={FILL_TARGET_S} policy_stable_window_s={STABLE_WINDOW_S} policy_promotion_min_ratio={PROMOTION_MIN_RATIO} policy_drop_min_ratio={DROP_MIN_RATIO} policy_underrun_backoff_s={UNDERRUN_BACKOFF_S} policy_conservative_window_s={CONSERVATIVE_WINDOW_S} policy_conservative_start_buffer_s={CONSERVATIVE_START_BUFFER_S} policy_conservative_abort_buffer_s={CONSERVATIVE_ABORT_BUFFER_S} policy_conservative_cooldown_s={CONSERVATIVE_COOLDOWN_S} tuning_status=heuristic");
}

pub(crate) fn expected_duty(kind: PreviewSourceKind) -> f64 {
    match kind {
        PreviewSourceKind::Debrid => 0.65,
        // The 25% fallback uses the same grid as the 50% default for matched comparisons.
        PreviewSourceKind::Usenet | PreviewSourceKind::Http => 0.8 * DEFAULT_CONSERVATIVE_DUTY,
        PreviewSourceKind::Unknown => 0.5,
    }
}

pub(crate) fn conservative_source(kind: PreviewSourceKind) -> bool {
    matches!(kind, PreviewSourceKind::Usenet | PreviewSourceKind::Http)
}

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
    http_background: bool,
) -> PreviewMode {
    if requested == PreviewMode::Off || kind == PreviewSourceKind::Unknown {
        PreviewMode::Off
    } else if (kind == PreviewSourceKind::Http && !http_background)
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

pub(crate) fn http_background_enabled() -> bool {
    std::env::var("AIOSTREAMS_PREVIEW_HTTP_BACKGROUND").as_deref() != Ok("off")
}

pub(crate) fn seekable_for_preview(seekable: bool, partially_seekable: bool) -> bool {
    seekable && !partially_seekable
}

pub(crate) struct Grid {
    pub slots: u32,
    pub step: f64,
    pub cost_s: f64,
    pub learned: bool,
    pub samples: usize,
}

pub(crate) fn grid_for(duration: f64, cost_s: f64, learned: bool, kind: PreviewSourceKind) -> Grid {
    let cost_s = if cost_s.is_finite() && cost_s > 0.0 {
        cost_s
    } else {
        0.9
    };
    let slots = (FILL_TARGET_S * expected_duty(kind) / cost_s)
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
        samples: 0,
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

pub(crate) fn cost_class(
    kind: PreviewSourceKind,
    range: &str,
    wide: bool,
    variant: &str,
    work: &str,
) -> String {
    format!(
        "{}:{range}:{}:{variant}:{work}",
        kind_name(kind),
        if wide { "wide" } else { "hd" }
    )
}

fn valid_class(class: &str) -> bool {
    let fields: Vec<_> = class.split(':').collect();
    fields.len() == 5
        && matches!(fields[0], "debrid" | "usenet" | "http")
        && matches!(fields[1], "sdr" | "hdr" | "dv")
        && matches!(fields[2], "hd" | "wide")
        && matches!(fields[3], "classic" | "fast")
        && matches!(fields[4], "background" | "demand")
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
            .filter(|file| {
                if file.version == 1 {
                    log::info!(target: "seek_preview", "grid_history=legacy-v1-ignored reason=mixed-demand-background-samples");
                }
                file.version == 2
            })
            .unwrap_or_else(|| CostFile {
                version: 2,
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

    pub fn grid(&self, duration: f64, class: &str, default: f64, kind: PreviewSourceKind) -> Grid {
        let learned = self
            .file
            .samples
            .get(class)
            .filter(|samples| samples.len() >= MIN_COST_SAMPLES);
        let cost = learned.map(|samples| {
            let mut sorted: Vec<_> = samples.iter().copied().collect();
            sorted.sort_unstable_by(f64::total_cmp);
            sorted[sorted.len() / 2]
        });
        let mut grid = grid_for(duration, cost.unwrap_or(default), cost.is_some(), kind);
        grid.samples = self.file.samples.get(class).map_or(0, VecDeque::len);
        grid
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
    pub ceiling: f64,
    pub gate_fail: Option<&'static str>,
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
    background_attempts: VecDeque<f64>,
    conservative_ceiling: f64,
}

impl Controller {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_ceiling(MIN_CONSERVATIVE_DUTY)
    }

    pub fn with_ceiling(conservative_ceiling: f64) -> Self {
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
            background_attempts: VecDeque::new(),
            conservative_ceiling: conservative_ceiling
                .clamp(MIN_CONSERVATIVE_DUTY, MAX_CONSERVATIVE_DUTY),
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

    pub fn background_attempt(&mut self, now: f64) {
        self.background_attempts.push_back(now);
    }

    pub fn update(&mut self, h: Health, kind: PreviewSourceKind, full: bool) -> Duty {
        let conservative = conservative_source(kind);
        let levels = if conservative {
            CONSERVATIVE_LEVELS
        } else {
            DEBRID_LEVELS
        };
        if conservative {
            self.ceiling = self.ceiling.min(
                levels
                    .iter()
                    .rposition(|v| *v <= self.conservative_ceiling)
                    .unwrap_or(0),
            );
        }
        let buffered = h.buffered.filter(|s| s.is_finite() && *s >= 0.0);
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
        // Input underrun alone also invalidates the conservative recovery window.
        if h.current && (h.buffering || h.underrun) {
            self.last_buffering = Some(h.at);
        }
        if cause == Some(BufferingCause::Underrun) {
            if (conservative || self.last_duty > 0.5) && self.level > 0 {
                self.ceiling = self.ceiling.min(self.level - 1);
            }
            self.level = self.level.saturating_sub(1).min(self.ceiling);
            self.throttle_until = h.at + UNDERRUN_BACKOFF_S;
        }
        if h.buffering && self.buffering_cause == Some(BufferingCause::Underrun) {
            self.throttle_until = self.throttle_until.max(h.at + UNDERRUN_BACKOFF_S);
        }
        if !h.buffering {
            self.buffering_cause = None;
        }
        if seek
            || h.buffering
            || h.underrun
            || !h.current
            || buffered.is_none()
            || self.previous.is_some_and(|p| h.at - p.at > 10.0)
        {
            self.samples.clear();
        } else if let Some(seconds) = buffered {
            self.samples.push_back((h.at, seconds));
            while self
                .samples
                .get(1)
                .is_some_and(|(at, _)| *at <= h.at - CONSERVATIVE_WINDOW_S)
            {
                self.samples.pop_front();
            }
            self.comfortable |= seconds >= MIN_BACKGROUND_BUFFER_S;
        }
        while self
            .background_attempts
            .front()
            .is_some_and(|at| h.at - *at >= 60.0)
        {
            self.background_attempts.pop_front();
        }
        let gate_fail = if conservative {
            if buffered.is_none_or(|s| s < CONSERVATIVE_START_BUFFER_S) {
                Some("buffer")
            } else if !self.complete_window(h.at, CONSERVATIVE_WINDOW_S) {
                Some("window")
            } else if self
                .minimum(h.at, CONSERVATIVE_WINDOW_S)
                .is_none_or(|s| s < CONSERVATIVE_START_BUFFER_S)
            {
                Some("buffer")
            } else if !h.reader_idle {
                Some("reader")
            } else if self
                .last_buffering
                .is_some_and(|at| h.at - at < CONSERVATIVE_COOLDOWN_S)
            {
                Some("recent-buffering")
            } else {
                None
            }
        } else {
            None
        };
        let health_allowed = full
            && h.current
            && !h.pointer
            && !seek
            && !h.buffering
            && !h.underrun
            && buffered.is_some_and(|s| s >= MIN_BACKGROUND_BUFFER_S)
            && gate_fail.is_none();
        let mut dropped = false;
        if !health_allowed {
            // A gated source must not climb to top duty before its first admission.
            self.stable_since = h.at;
            self.baseline = None;
        } else {
            if self.baseline.is_none() {
                self.stable_since = h.at;
            }
            let minimum = self.minimum(h.at, STABLE_WINDOW_S).unwrap_or(0.0);
            let previous_min = self.baseline.get_or_insert(minimum);
            if h.at >= self.seek_until && minimum < *previous_min * DROP_MIN_RATIO {
                self.level = self.level.saturating_sub(1);
                dropped = true;
                *previous_min = minimum;
                self.stable_since = h.at;
            } else if h.at - self.stable_since >= STABLE_WINDOW_S && h.at >= self.throttle_until {
                if minimum >= MIN_BACKGROUND_BUFFER_S
                    && minimum >= *previous_min * PROMOTION_MIN_RATIO
                {
                    self.level = (self.level + 1).min(self.ceiling);
                }
                *previous_min = minimum;
                self.stable_since = h.at;
            }
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
        } else if buffered.is_none_or(|s| s < MIN_BACKGROUND_BUFFER_S) {
            "buffer-low-or-unknown"
        } else if gate_fail.is_some() {
            if kind == PreviewSourceKind::Usenet {
                "usenet-health"
            } else {
                "http-health"
            }
        } else if conservative
            && self.background_attempts.len() >= seek_cap(self.conservative_ceiling)
        {
            if kind == PreviewSourceKind::Usenet {
                "usenet-rate-limit"
            } else {
                "http-rate-limit"
            }
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
        } else if throttled {
            0.25
        } else {
            levels[self.level]
        };
        self.previous = Some(h);
        self.last_duty = value;
        Duty {
            value,
            level: levels[self.level],
            reason,
            cause,
            at_buffering,
            throttled,
            ceiling: levels[self.ceiling],
            gate_fail,
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
    fn source_duty_grids_are_bounded_and_measured_costs_change_only_later_grids() {
        let lotr = grid_for(13692.0, 1.02, true, PreviewSourceKind::Debrid);
        assert_eq!(lotr.slots, 306);
        assert_eq!(lotr.step, 44.746);
        assert_eq!(
            grid_for(1824.0, 0.657, true, PreviewSourceKind::Debrid).step,
            5.0
        );
        assert_eq!(
            grid_for(1420.0, 0.33, true, PreviewSourceKind::Debrid).step,
            5.0
        );
        let usenet = grid_for(8700.0, 0.684, true, PreviewSourceKind::Usenet);
        assert_eq!(usenet.slots, 281);
        assert_eq!(usenet.step, 30.961);
        for cost in [0.0, f64::NAN, f64::INFINITY, 0.001, 100.0] {
            let grid = grid_for(86400.0, cost, false, PreviewSourceKind::Http);
            assert!((240..=600).contains(&grid.slots));
            assert!(grid.step <= 360.0);
        }
        let mut costs = Costs::load(None);
        let class = cost_class(
            PreviewSourceKind::Debrid,
            "dv",
            true,
            "classic",
            "background",
        );
        let original = costs.grid(13692.0, &class, 0.9, PreviewSourceKind::Debrid);
        for cost in [0.9, 1.0, 1.02, 1.04, 8.0].into_iter().cycle().take(9) {
            costs.observe(&class, cost);
        }
        assert!(
            !costs
                .grid(13692.0, &class, 0.9, PreviewSourceKind::Debrid)
                .learned
        );
        costs.observe(&class, 8.0);
        let learned = costs.grid(13692.0, &class, 0.9, PreviewSourceKind::Debrid);
        assert!(!original.learned);
        assert_eq!(original.slots, 347);
        assert_eq!(learned.slots, 306);
        assert_eq!(learned.samples, 10);
        for _ in 0..100 {
            costs.observe(&class, 0.75);
        }
        costs.observe("not:a:class", 0.75);
        costs.observe(&class, f64::NAN);
        assert_eq!(costs.file.samples[&class].len(), COST_SAMPLES);
        assert_eq!(costs.file.samples.len(), 1);
        for (variant, work) in [("fast", "background"), ("classic", "demand")] {
            let separate = cost_class(PreviewSourceKind::Debrid, "dv", true, variant, work);
            assert!(
                !costs
                    .grid(13692.0, &separate, 0.9, PreviewSourceKind::Debrid)
                    .learned
            );
        }
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
            c.background_attempt(60.0);
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
    fn fallback_ceiling_never_promotes_and_stage_two_cap_is_rolling() {
        for kind in [PreviewSourceKind::Usenet, PreviewSourceKind::Http] {
            let mut baseline = Controller::new();
            for at in 0..=300 {
                let d = baseline.update(health(f64::from(at), 80.0), kind, true);
                assert_eq!(d.value, if at < 60 { 0.0 } else { 0.25 });
                assert_eq!(d.ceiling, 0.25);
            }
            let mut trial = Controller::with_ceiling(0.5);
            for at in 0..=120 {
                trial.update(health(f64::from(at), 80.0), kind, true);
            }
            for _ in 0..59 {
                trial.background_attempt(120.0);
            }
            assert_eq!(trial.update(health(121.0, 80.0), kind, true).value, 0.5);
            trial.background_attempt(121.0);
            assert!(
                trial
                    .update(health(122.0, 80.0), kind, true)
                    .reason
                    .ends_with("rate-limit")
            );
            // Keep the health history continuous while the rolling window expires.
            for at in 123..=179 {
                trial.update(health(f64::from(at), 80.0), kind, true);
            }
            assert_eq!(trial.update(health(180.0, 80.0), kind, true).value, 0.5);
        }
        assert_eq!(seek_cap(0.25), 30);
        assert_eq!(seek_cap(0.5), 60);
    }

    #[test]
    fn source_and_timeout_gates_are_native_and_bounded() {
        assert_eq!(
            enforced_mode(PreviewSourceKind::Http, PreviewMode::Full, true, false),
            PreviewMode::DemandOnly
        );
        assert_eq!(
            enforced_mode(PreviewSourceKind::Usenet, PreviewMode::Full, false, true),
            PreviewMode::DemandOnly
        );
        assert_eq!(
            enforced_mode(PreviewSourceKind::Unknown, PreviewMode::Full, true, true),
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
        let class = "debrid:hdr:wide:classic:background";
        let mut costs = Costs::load(Some(&dir));
        for _ in 0..10 {
            costs.observe(class, 0.78);
        }
        costs.save();
        assert!(
            Costs::load(Some(&dir))
                .grid(13692.0, class, 0.9, PreviewSourceKind::Debrid)
                .learned
        );
        std::fs::write(dir.join("preview-seek-costs.json"), b"invalid").unwrap();
        assert!(
            !Costs::load(Some(&dir))
                .grid(13692.0, class, 0.9, PreviewSourceKind::Debrid)
                .learned
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn conservative_sources_ramp_only_after_admission_and_stop_on_underrun() {
        for kind in [PreviewSourceKind::Usenet, PreviewSourceKind::Http] {
            let mut c = Controller::with_ceiling(0.5);
            for at in 0..=150 {
                let d = c.update(health(f64::from(at), 80.0), kind, true);
                assert_eq!(
                    d.value,
                    if at < 60 {
                        0.0
                    } else if at < 90 {
                        0.25
                    } else if at < 120 {
                        0.35
                    } else {
                        0.5
                    }
                );
                assert_eq!(d.ceiling, 0.5);
            }
            let mut h = health(151.0, 80.0);
            h.underrun = true;
            let d = c.update(h, kind, true);
            assert_eq!(d.value, 0.0);
            assert_eq!(d.cause, Some(BufferingCause::Underrun));
            assert_eq!(d.ceiling, 0.35);
            for at in 152..=510 {
                let d = c.update(health(f64::from(at), 80.0), kind, true);
                assert!(d.value <= 0.35);
                if at < 451 {
                    assert_eq!(d.value, 0.0);
                }
            }
        }
    }

    #[test]
    fn shallow_or_busy_sources_do_not_disable_demand_and_can_recover() {
        for kind in [PreviewSourceKind::Usenet, PreviewSourceKind::Http] {
            let mut c = Controller::new();
            for at in 0..=80 {
                let d = c.update(health(f64::from(at), 25.0), kind, true);
                assert_eq!(d.value, 0.0);
                assert_eq!(d.gate_fail, Some("buffer"));
            }
            for at in 81..=200 {
                let mut h = health(f64::from(at), 80.0);
                h.reader_idle = false;
                let d = c.update(h, kind, true);
                assert_eq!(d.value, 0.0);
                if at >= 141 {
                    assert_eq!(d.gate_fail, Some("reader"));
                }
            }
            let d = c.update(health(201.0, 80.0), kind, true);
            assert_eq!(d.value, 0.25);
            assert_eq!(d.gate_fail, None);
            assert_eq!(
                enforced_mode(kind, PreviewMode::Full, true, true),
                PreviewMode::Full
            );
            assert_eq!(
                c.update(health(202.0, 80.0), kind, false).reason,
                "demand-only"
            );
        }
    }

    #[test]
    fn legacy_mixed_cost_history_is_not_used_as_new_background_evidence() {
        let dir =
            std::env::temp_dir().join(format!("aio-preview-legacy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("preview-seek-costs.json"), br#"{"version":1,"samples":{"debrid:dv:wide":[0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5]}}"#).unwrap();
        let costs = Costs::load(Some(&dir));
        assert!(costs.file.samples.is_empty());
        assert_eq!(costs.file.version, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
