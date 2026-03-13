//! Agent load tracking for delegation and subagent orchestration.
//!
//! Stub implementation - this module is referenced but not yet fully implemented.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default)]
pub struct AgentLoadSnapshot {
    pub in_flight: usize,
    pub recent_assignments: usize,
    pub recent_failures: usize,
}

#[derive(Clone)]
pub struct AgentLoadTracker {
    inner: Arc<Mutex<AgentLoadTrackerInner>>,
}

struct AgentLoadTrackerInner {
    snapshots: HashMap<String, AgentLoadSnapshot>,
}

impl AgentLoadTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AgentLoadTrackerInner {
                snapshots: HashMap::new(),
            })),
        }
    }

    pub fn snapshot(&self, _window: Duration) -> HashMap<String, AgentLoadSnapshot> {
        self.inner.lock().unwrap().snapshots.clone()
    }

    pub fn start(&self, agent_name: &str) -> LoadLease {
        let mut inner = self.inner.lock().unwrap();
        let snapshot = inner.snapshots.entry(agent_name.to_string()).or_default();
        snapshot.in_flight += 1;

        LoadLease {
            agent_name: agent_name.to_string(),
            tracker: self.clone(),
        }
    }

    pub fn record_failure(&self, agent_name: &str) {
        let mut inner = self.inner.lock().unwrap();
        let snapshot = inner.snapshots.entry(agent_name.to_string()).or_default();
        snapshot.recent_failures += 1;
    }
}

pub struct LoadLease {
    agent_name: String,
    tracker: AgentLoadTracker,
}

impl LoadLease {
    pub fn mark_success(&mut self) {
        let mut inner = self.tracker.inner.lock().unwrap();
        let snapshot = inner.snapshots.entry(self.agent_name.clone()).or_default();
        if snapshot.in_flight > 0 {
            snapshot.in_flight -= 1;
        }
        snapshot.recent_assignments += 1;
    }

    pub fn mark_failure(&mut self) {
        let mut inner = self.tracker.inner.lock().unwrap();
        let snapshot = inner.snapshots.entry(self.agent_name.clone()).or_default();
        if snapshot.in_flight > 0 {
            snapshot.in_flight -= 1;
        }
        snapshot.recent_failures += 1;
    }
}

impl Drop for LoadLease {
    fn drop(&mut self) {
        // Default to marking as failure if not explicitly marked
    }
}
