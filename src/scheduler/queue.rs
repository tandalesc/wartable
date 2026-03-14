use crate::models::{Job, JobId};
use std::collections::VecDeque;

/// Priority queue for jobs. Sorts by priority (desc) then submission time (asc).
pub struct JobQueue {
    jobs: VecDeque<Job>,
}

impl JobQueue {
    pub fn new() -> Self {
        Self {
            jobs: VecDeque::new(),
        }
    }

    /// Insert a job in priority order.
    pub fn push(&mut self, job: Job) {
        let pos = self
            .jobs
            .iter()
            .position(|existing| {
                // Insert before first job with lower priority,
                // or same priority but later submission time
                existing.spec.priority < job.spec.priority
                    || (existing.spec.priority == job.spec.priority
                        && existing.submitted_at > job.submitted_at)
            })
            .unwrap_or(self.jobs.len());
        self.jobs.insert(pos, job);
    }

    /// Remove and return the next job to schedule (highest priority, earliest submission).
    pub fn pop(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    /// Peek at the queue without removing.
    pub fn peek(&self) -> Option<&Job> {
        self.jobs.front()
    }

    /// Remove a specific job by ID. Returns the job if found.
    pub fn remove(&mut self, job_id: &JobId) -> Option<Job> {
        if let Some(pos) = self.jobs.iter().position(|j| j.id == *job_id) {
            self.jobs.remove(pos)
        } else {
            None
        }
    }

    /// Get position of a job in the queue (0-indexed).
    pub fn position(&self, job_id: &JobId) -> Option<usize> {
        self.jobs.iter().position(|j| j.id == *job_id)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter()
    }
}
