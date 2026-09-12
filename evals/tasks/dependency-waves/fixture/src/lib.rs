#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub dependencies: Vec<String>,
    pub resources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    ZeroCapacity,
    EmptyId,
    DuplicateId(String),
    UnknownDependency { job: String, dependency: String },
    UnknownCompleted(String),
    Blocked(Vec<String>),
}

pub fn plan(
    jobs: &[Job],
    completed: &[String],
    capacity: usize,
) -> Result<Vec<Vec<String>>, PlanError> {
    if capacity == 0 {
        return Err(PlanError::ZeroCapacity);
    }
    Ok(jobs
        .iter()
        .filter(|job| !completed.contains(&job.id))
        .map(|job| vec![job.id.clone()])
        .collect())
}
