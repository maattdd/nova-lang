//! Phase timing profiler, enabled with `--profile`.
//!
//! `start`/`end` are balanced and may nest (e.g. per-`@import` sub-phases
//! inside "macro expand"). Times accumulate per phase name; `report` prints
//! them in first-use order when profiling is enabled.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct Profiler {
    enabled: bool,
    stack: RefCell<Vec<(String, Instant)>>,
    phases: RefCell<HashMap<String, Duration>>,
    order: RefCell<Vec<String>>,
}

impl Profiler {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            stack: RefCell::new(Vec::new()),
            phases: RefCell::new(HashMap::new()),
            order: RefCell::new(Vec::new()),
        }
    }

    /// Begin timing a phase (nestable).
    pub fn start(&self, name: &str) {
        if !self.enabled {
            return;
        }
        self.stack.borrow_mut().push((name.to_string(), Instant::now()));
    }

    /// End the innermost open phase and accumulate its elapsed time.
    pub fn end(&self) {
        if !self.enabled {
            return;
        }
        let mut stack = self.stack.borrow_mut();
        if let Some((name, start)) = stack.pop() {
            let elapsed = start.elapsed();
            let mut phases = self.phases.borrow_mut();
            let entry = phases.entry(name.clone()).or_insert_with(|| {
                self.order.borrow_mut().push(name.clone());
                Duration::ZERO
            });
            *entry += elapsed;
        }
    }

    /// Print the phase timings (no-op when disabled).
    pub fn report(&self) {
        if !self.enabled {
            return;
        }
        let order = self.order.borrow();
        let phases = self.phases.borrow();
        eprintln!("── phase profile ──");
        for name in order.iter() {
            if let Some(d) = phases.get(name) {
                eprintln!("  {:<16} {:>10.3} ms", name, d.as_secs_f64() * 1000.0);
            }
        }
        let total: Duration = phases.values().sum();
        eprintln!("  {:<16} {:>10.3} ms", "total", total.as_secs_f64() * 1000.0);
    }
}
