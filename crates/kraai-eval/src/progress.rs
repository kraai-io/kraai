use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct ProgressReporter {
    inner: Arc<Mutex<ProgressSnapshot>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub task_id: String,
    pub harness_name: String,
    pub runner_version: String,
    pub model_label: Option<String>,
    pub attempt: u64,
    pub phase: String,
}

impl ProgressReporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        self.inner
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }

    pub(crate) fn initialize(
        &self,
        task_id: &str,
        harness_name: &str,
        runner_version: &str,
        model_label: Option<&str>,
        attempt: u64,
    ) {
        if let Ok(mut snapshot) = self.inner.lock() {
            *snapshot = ProgressSnapshot {
                task_id: task_id.to_owned(),
                harness_name: harness_name.to_owned(),
                runner_version: runner_version.to_owned(),
                model_label: model_label.map(str::to_owned),
                attempt,
                phase: String::from("preparing evaluation"),
            };
        }
    }

    pub(crate) fn set_phase(&self, phase: impl Into<String>) {
        if let Ok(mut snapshot) = self.inner.lock() {
            snapshot.phase = phase.into();
        }
    }
}
