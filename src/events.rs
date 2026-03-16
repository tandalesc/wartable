use crate::models::Event;
use tokio::sync::broadcast;

/// Broadcast channel for live dashboard updates.
/// Subscribers (WebSocket connections) receive cloned events.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, event: Event) {
        // Ignore error (no subscribers)
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use chrono::Utc;

    fn make_job_info() -> JobInfo {
        JobInfo {
            job_id: "test-1".to_string(),
            name: Some("test".to_string()),
            status: JobStatus::Queued,
            command: "echo hi".to_string(),
            submitted_at: Utc::now(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            tags: vec![],
        }
    }

    #[tokio::test]
    async fn publish_and_receive() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        bus.publish(Event::JobSubmitted {
            job: make_job_info(),
        });

        let event = rx.recv().await.unwrap();
        match event {
            Event::JobSubmitted { job } => assert_eq!(job.job_id, "test-1"),
            _ => panic!("unexpected event type"),
        }
    }

    #[tokio::test]
    async fn publish_without_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        // No subscribers — should not panic
        bus.publish(Event::JobSubmitted {
            job: make_job_info(),
        });
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.publish(Event::JobStarted {
            job: make_job_info(),
        });

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        match (e1, e2) {
            (Event::JobStarted { job: j1 }, Event::JobStarted { job: j2 }) => {
                assert_eq!(j1.job_id, j2.job_id);
            }
            _ => panic!("unexpected event types"),
        }
    }
}
