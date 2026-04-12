use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{self, Duration};

pub struct RateLimiter {
    semaphore: Arc<Semaphore>,
    _refill_handle: tokio::task::JoinHandle<()>,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32) -> Self {
        Self::with_config(requests_per_minute, 2, 100)
    }

    /// Create a rate limiter with explicit burst divisor and minimum interval.
    pub fn with_config(
        requests_per_minute: u32,
        burst_divisor: u32,
        min_interval_ms: u64,
    ) -> Self {
        let burst = (requests_per_minute as usize / burst_divisor.max(1) as usize).max(1);
        let semaphore = Arc::new(Semaphore::new(burst));

        let sem_clone = semaphore.clone();
        let interval_ms = (60_000 / requests_per_minute as u64).max(min_interval_ms);

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
