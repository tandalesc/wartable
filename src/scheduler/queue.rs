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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_job(id: &str, priority: i32, submitted_ms_offset: i64) -> Job {
        Job {
            id: id.to_string(),
            spec: JobSpec {
                command: format!("echo {}", id),
                working_dir: None,
                env: HashMap::new(),
                resources: ResourceRequirements::default(),
                files: Vec::new(),
                priority,
                tags: Vec::new(),
                name: None,
            },
            status: JobStatus::Queued,
            submitted_at: Utc::now() + chrono::Duration::milliseconds(submitted_ms_offset),
            started_at: None,
            completed_at: None,
            exit_code: None,
            pid: None,
        }
    }

    #[test]
    fn empty_queue() {
        let mut q = JobQueue::new();
        assert_eq!(q.len(), 0);
        assert!(q.pop().is_none());
        assert!(q.peek().is_none());
    }

    #[test]
    fn single_job() {
        let mut q = JobQueue::new();
        q.push(make_job("a", 0, 0));
        assert_eq!(q.len(), 1);
        assert_eq!(q.peek().unwrap().id, "a");
        let job = q.pop().unwrap();
        assert_eq!(job.id, "a");
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn fifo_same_priority() {
        let mut q = JobQueue::new();
        q.push(make_job("first", 0, 0));
        q.push(make_job("second", 0, 100));
        q.push(make_job("third", 0, 200));

        assert_eq!(q.pop().unwrap().id, "first");
        assert_eq!(q.pop().unwrap().id, "second");
        assert_eq!(q.pop().unwrap().id, "third");
    }

    #[test]
    fn higher_priority_jumps_queue() {
        let mut q = JobQueue::new();
        q.push(make_job("low", 0, 0));
        q.push(make_job("high", 10, 100));
        q.push(make_job("med", 5, 200));

        assert_eq!(q.pop().unwrap().id, "high");
        assert_eq!(q.pop().unwrap().id, "med");
        assert_eq!(q.pop().unwrap().id, "low");
    }

    #[test]
    fn mixed_priority_and_time() {
        let mut q = JobQueue::new();
        q.push(make_job("p0_early", 0, 0));
        q.push(make_job("p0_late", 0, 100));
        q.push(make_job("p5_early", 5, 50));
        q.push(make_job("p5_late", 5, 150));

        assert_eq!(q.pop().unwrap().id, "p5_early");
        assert_eq!(q.pop().unwrap().id, "p5_late");
        assert_eq!(q.pop().unwrap().id, "p0_early");
        assert_eq!(q.pop().unwrap().id, "p0_late");
    }

    #[test]
    fn remove_by_id() {
        let mut q = JobQueue::new();
        q.push(make_job("a", 0, 0));
        q.push(make_job("b", 0, 100));
        q.push(make_job("c", 0, 200));

        let removed = q.remove(&"b".to_string());
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, "b");
        assert_eq!(q.len(), 2);

        assert_eq!(q.pop().unwrap().id, "a");
        assert_eq!(q.pop().unwrap().id, "c");
    }

    #[test]
    fn remove_nonexistent() {
        let mut q = JobQueue::new();
        q.push(make_job("a", 0, 0));
        assert!(q.remove(&"z".to_string()).is_none());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn position() {
        let mut q = JobQueue::new();
        q.push(make_job("a", 10, 0));
        q.push(make_job("b", 5, 100));
        q.push(make_job("c", 0, 200));

        assert_eq!(q.position(&"a".to_string()), Some(0));
        assert_eq!(q.position(&"b".to_string()), Some(1));
        assert_eq!(q.position(&"c".to_string()), Some(2));
        assert_eq!(q.position(&"z".to_string()), None);
    }

    #[test]
    fn negative_priority() {
        let mut q = JobQueue::new();
        q.push(make_job("neg", -5, 0));
        q.push(make_job("zero", 0, 100));
        q.push(make_job("pos", 5, 200));

        assert_eq!(q.pop().unwrap().id, "pos");
        assert_eq!(q.pop().unwrap().id, "zero");
        assert_eq!(q.pop().unwrap().id, "neg");
    }
}
