//! Diagnostics for investigating WM freezes.
//!
//! Tracks how long each event takes to be processed, detects event floods
//! and runs a watchdog thread that logs when the main loop is stuck on a
//! single event. Logs go to `~/.glzr/glazewm/diagnostics.log`.

use std::{
  collections::HashMap,
  sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
  },
  time::{Duration, Instant},
};

/// Log target of freeze diagnostics. Logs with this target are written to
/// `diagnostics.log` from the `INFO` level.
pub const LOG_TARGET: &str = "glazewm::diagnostics";

/// Processing time above which an event is logged as slow.
const SLOW_EVENT_THRESHOLD: Duration = Duration::from_millis(250);

/// Processing time above which the main loop is considered stalled.
const STALL_THRESHOLD: Duration = Duration::from_secs(3);

/// Interval at which the stall detector checks the main loop.
const STALL_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Number of events per second above which a flood is reported.
const FLOOD_THRESHOLD_PER_SEC: u32 = 300;

/// State shared between the main loop and the stall detector thread.
struct SharedState {
  epoch: Instant,
  /// Milliseconds since `epoch` at which the current event started
  /// processing, offset by 1 so that 0 means idle.
  busy_since_ms: AtomicU64,
  /// Description of the event currently being processed.
  current_event: Mutex<String>,
}

/// Tracks event processing of the WM's main loop.
pub struct EventDiagnostics {
  shared: Arc<SharedState>,
  flood_counter: FloodCounter,
}

impl EventDiagnostics {
  /// Creates the diagnostics and spawns the stall detector thread.
  pub fn start() -> Self {
    let shared = Arc::new(SharedState {
      epoch: Instant::now(),
      busy_since_ms: AtomicU64::new(0),
      current_event: Mutex::new(String::new()),
    });

    let detector_state = shared.clone();
    let spawn_res = std::thread::Builder::new()
      .name("wm-stall-detector".to_string())
      .spawn(move || run_stall_detector(&detector_state));

    if let Err(err) = spawn_res {
      tracing::warn!("Failed to start stall detector: {err}");
    }

    Self {
      shared,
      flood_counter: FloodCounter::new(Instant::now()),
    }
  }

  /// Runs `process` while tracking its duration.
  ///
  /// `kind` is a short category used for flood reports, and `detail` a
  /// description of the event used for slow and stall reports.
  pub fn track<T>(
    &mut self,
    kind: &'static str,
    detail: String,
    process: impl FnOnce() -> T,
  ) -> T {
    let start = Instant::now();

    if let Some(report) = self.flood_counter.record(kind, start) {
      tracing::warn!("Event flood: {report}.");
    }

    if let Ok(mut current_event) = self.shared.current_event.lock() {
      *current_event = detail;
    }

    self
      .shared
      .busy_since_ms
      .store(self.shared.millis_since_epoch(start) + 1, Ordering::Release);

    let res = process();

    self.shared.busy_since_ms.store(0, Ordering::Release);

    let elapsed = start.elapsed();
    if elapsed > SLOW_EVENT_THRESHOLD {
      let detail = self
        .shared
        .current_event
        .lock()
        .map(|current_event| current_event.clone())
        .unwrap_or_default();

      tracing::warn!("Slow {kind} processing ({elapsed:?}): {detail}");
    }

    res
  }
}

impl SharedState {
  /// Gets the milliseconds elapsed between `epoch` and `instant`.
  fn millis_since_epoch(&self, instant: Instant) -> u64 {
    u64::try_from(instant.duration_since(self.epoch).as_millis())
      .unwrap_or(u64::MAX - 1)
  }
}

/// Periodically checks whether the main loop is stuck on an event.
///
/// Logs once per stalled event, then again every `STALL_THRESHOLD` while
/// it stays stuck.
fn run_stall_detector(shared: &SharedState) {
  let mut last_report: Option<(u64, Duration)> = None;

  loop {
    std::thread::sleep(STALL_CHECK_INTERVAL);

    let busy_since_ms = shared.busy_since_ms.load(Ordering::Acquire);
    let now_ms = shared.millis_since_epoch(Instant::now()) + 1;

    let Some(stall) = stall_duration(busy_since_ms, now_ms) else {
      last_report = None;
      continue;
    };

    let should_report = match last_report {
      Some((reported_since, reported_stall)) => {
        reported_since != busy_since_ms
          || stall >= reported_stall + STALL_THRESHOLD
      }
      None => true,
    };

    if should_report {
      let detail = shared
        .current_event
        .lock()
        .map(|current_event| current_event.clone())
        .unwrap_or_default();

      tracing::warn!(
        "WM main loop stalled for {stall:?} while processing: {detail}"
      );

      last_report = Some((busy_since_ms, stall));
    }
  }
}

/// Gets how long the main loop has been stuck, if above the stall
/// threshold.
///
/// `busy_since_ms` is 0 when idle; both values are offset by 1.
fn stall_duration(busy_since_ms: u64, now_ms: u64) -> Option<Duration> {
  if busy_since_ms == 0 {
    return None;
  }

  let stall = Duration::from_millis(now_ms.saturating_sub(busy_since_ms));
  (stall >= STALL_THRESHOLD).then_some(stall)
}

/// Counts events per one-second window to detect floods.
struct FloodCounter {
  window_start: Instant,
  counts: HashMap<&'static str, u32>,
}

impl FloodCounter {
  /// Creates a counter with a window starting at `now`.
  fn new(now: Instant) -> Self {
    Self {
      window_start: now,
      counts: HashMap::new(),
    }
  }

  /// Records an event of the given kind.
  ///
  /// Returns a report of the previous window if it was a flood.
  fn record(&mut self, kind: &'static str, now: Instant) -> Option<String> {
    let mut report = None;
    let elapsed = now.duration_since(self.window_start);

    if elapsed >= Duration::from_secs(1) {
      let total = self.counts.values().sum::<u32>();

      if total > FLOOD_THRESHOLD_PER_SEC {
        let mut counts = self.counts.iter().collect::<Vec<_>>();
        counts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));

        let breakdown = counts
          .iter()
          .map(|(kind, count)| format!("{kind}={count}"))
          .collect::<Vec<_>>()
          .join(", ");

        report = Some(format!(
          "{total} events in {elapsed:.1?} ({breakdown})"
        ));
      }

      self.counts.clear();
      self.window_start = now;
    }

    *self.counts.entry(kind).or_insert(0) += 1;
    report
  }
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use super::{stall_duration, FloodCounter, FLOOD_THRESHOLD_PER_SEC};

  #[test]
  fn detects_stalls_above_threshold() {
    assert_eq!(stall_duration(0, 10_000), None);
    assert_eq!(stall_duration(1_000, 2_000), None);
    assert_eq!(
      stall_duration(1_000, 5_000),
      Some(Duration::from_millis(4_000))
    );
  }

  #[test]
  fn reports_floods_once_per_window() {
    let start = Instant::now();
    let mut counter = FloodCounter::new(start);

    for _ in 0..=FLOOD_THRESHOLD_PER_SEC {
      assert_eq!(counter.record("window", start), None);
    }
    assert_eq!(counter.record("mouse", start), None);

    let report = counter
      .record("window", start + Duration::from_secs(1))
      .expect("flood should be reported");
    assert!(report.starts_with(&format!(
      "{} events",
      FLOOD_THRESHOLD_PER_SEC + 2
    )));
    assert!(report.contains(&format!(
      "window={}, mouse=1",
      FLOOD_THRESHOLD_PER_SEC + 1
    )));

    // New window only contains the last event.
    assert_eq!(counter.record("window", start + Duration::from_secs(2)), None);
  }

  #[test]
  fn ignores_normal_event_rates() {
    let start = Instant::now();
    let mut counter = FloodCounter::new(start);

    for _ in 0..10 {
      counter.record("keybinding", start);
    }

    assert_eq!(counter.record("window", start + Duration::from_secs(1)), None);
  }
}
