use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{self, Duration};

pub struct RateLimiter {
    semaphore: Arc<Semaphore>,
    _refill_handle: tokio::task::JoinHandle<()>,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32) -> Self {
        let burst = (requests_per_minute as usize / 2).max(1);
        let semaphore = Arc::new(Semaphore::new(burst));

        let sem_clone = semaphore.clone();
        let interval_ms = (60_000 / requests_per_minute as u64).max(100);

        let handle = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(interval_ms));
            loop {
                interval.tick().await;
                if sem_clone.available_permits() < burst {
                    sem_clone.add_permits(1);
                }
            }
        });

        Self {
            semaphore,
            _refill_handle: handle,
        }
    }

    pub async fn acquire(&self) {
        let permit = self.semaphore.acquire().await;
        if let Ok(permit) = permit {
            permit.forget();
        }
    }
}
