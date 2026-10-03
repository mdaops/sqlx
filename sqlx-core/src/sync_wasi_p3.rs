//! The same runtime-independent semaphore SQLx uses for its non-Tokio backends.
pub struct AsyncSemaphore {
    inner: futures_intrusive::sync::Semaphore,
}
impl AsyncSemaphore {
    pub fn new(fair: bool, permits: usize) -> Self {
        Self {
            inner: futures_intrusive::sync::Semaphore::new(fair, permits),
        }
    }
    pub fn permits(&self) -> usize {
        self.inner.permits()
    }
    pub async fn acquire(&self, permits: u32) -> AsyncSemaphoreReleaser<'_> {
        AsyncSemaphoreReleaser {
            inner: self.inner.acquire(permits as usize).await,
        }
    }
    pub fn try_acquire(&self, permits: u32) -> Option<AsyncSemaphoreReleaser<'_>> {
        Some(AsyncSemaphoreReleaser {
            inner: self.inner.try_acquire(permits as usize)?,
        })
    }
    pub fn release(&self, permits: usize) {
        self.inner.release(permits);
    }
}
pub struct AsyncSemaphoreReleaser<'a> {
    inner: futures_intrusive::sync::SemaphoreReleaser<'a>,
}
impl AsyncSemaphoreReleaser<'_> {
    pub fn disarm(mut self) {
        self.inner.disarm();
    }
}
