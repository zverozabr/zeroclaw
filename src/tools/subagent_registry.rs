//! Sub-agent session registry for tracking background agent execution.
//!
//! Stub implementation - this module is referenced but not yet fully implemented.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

use super::traits::ToolResult;

#[derive(Debug, Clone)]
pub enum SubAgentStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone)]
pub struct SubAgentSession {
    pub id: String,
    pub agent_name: String,
    pub task: String,
    pub status: SubAgentStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<ToolResult>,
    pub handle: Option<Arc<JoinHandle<()>>>,
}

pub struct SubAgentRegistry {
    inner: Arc<Mutex<SubAgentRegistryInner>>,
}

struct SubAgentRegistryInner {
    sessions: HashMap<String, SubAgentSession>,
}

impl SubAgentRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SubAgentRegistryInner {
                sessions: HashMap::new(),
            })),
        }
    }

    pub fn insert(&self, session: SubAgentSession) {
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.insert(session.id.clone(), session);
    }

    pub fn try_insert(
        &self,
        session: SubAgentSession,
        max_concurrent: usize,
    ) -> Result<(), (usize, Box<SubAgentSession>)> {
        let mut inner = self.inner.lock().unwrap();
        let running = inner
            .sessions
            .values()
            .filter(|s| matches!(s.status, SubAgentStatus::Running))
            .count();

        if running >= max_concurrent {
            Err((running, Box::new(session)))
        } else {
            inner.sessions.insert(session.id.clone(), session);
            Ok(())
        }
    }

    pub fn running_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner
            .sessions
            .values()
            .filter(|s| matches!(s.status, SubAgentStatus::Running))
            .count()
    }

    pub fn complete(&self, session_id: &str, result: ToolResult) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(session_id) {
            session.status = SubAgentStatus::Completed;
            session.completed_at = Some(Utc::now());
            session.result = Some(result);
        }
    }

    pub fn fail(&self, session_id: &str, error: String) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(session_id) {
            session.status = SubAgentStatus::Failed;
            session.completed_at = Some(Utc::now());
            session.result = Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }
    }

    pub fn set_handle(&self, session_id: &str, handle: JoinHandle<()>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(session_id) {
            session.handle = Some(Arc::new(handle));
        }
    }
}
