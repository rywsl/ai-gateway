//! Bounded-restart supervisor — filled in by task 1.2.

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("task returned error: {0}")]
    Task(#[from] anyhow::Error),
}

pub struct RestartPolicy;
pub struct Supervisor;
