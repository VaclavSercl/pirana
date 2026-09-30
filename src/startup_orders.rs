//! Read-only order recovery. Never cancels orders or changes accounting state.
use pirana_core::errors::{PiranaError, PiranaResult};
use std::future::Future;
use std::time::Duration;

/// A known pending order is a hard block; a failed read is never an empty observation.
/// After accounting recovery, require a fresh empty result, retrying read errors at most three times.
pub async fn verify<F, Fut, H>(
    initial: PiranaResult<bool>, mut read_empty: F, mut heartbeat: H,
    retry_delay: Duration,
) -> PiranaResult<()>
where F: FnMut() -> Fut, Fut: Future<Output = PiranaResult<bool>>, H: FnMut(),
{
    if matches!(initial, Ok(false)) {
        return Err(PiranaError::Config("active exchange orders require reconciliation before startup".into()));
    }
    for attempt in 0..3 {
        heartbeat();
        match read_empty().await {
            Ok(true) => return Ok(()),
            Ok(false) => return Err(PiranaError::Config("exchange orders changed during recovery".into())),
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => {
                tracing::warn!("Startup order observation unavailable; keeping execution blocked until fresh verification");
                tokio::time::sleep(retry_delay).await;
            }
        }
    }
    unreachable!("bounded recovery loop always returns")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    fn unavailable() -> PiranaResult<bool> { Err(PiranaError::Timeout("fixture".into())) }
    async fn run(initial: PiranaResult<bool>, values: Vec<PiranaResult<bool>>) -> (bool, usize) {
        let mut queue = VecDeque::from(values);
        let mut calls = 0;
        let result = verify(initial, || {
            calls += 1;
            std::future::ready(queue.pop_front().expect("unexpected request"))
        }, || {}, Duration::ZERO).await;
        (result.is_ok(), calls)
    }
    #[tokio::test]
    async fn transient_initial_failure_requires_and_accepts_fresh_empty_orders() {
        assert_eq!(run(unavailable(), vec![Ok(true)]).await, (true, 1));
    }
    #[tokio::test]
    async fn known_initial_pending_orders_remain_blocked() {
        assert_eq!(run(Ok(false), vec![]).await, (false, 0));
    }
    #[tokio::test]
    async fn newly_pending_orders_are_never_ignored() {
        assert_eq!(run(Ok(true), vec![Ok(false)]).await, (false, 1));
    }
    #[tokio::test]
    async fn unavailable_fresh_checks_are_bounded_and_fail_closed() {
        assert_eq!(run(Ok(true), vec![unavailable(), unavailable(), unavailable()]).await, (false, 3));
    }
    #[tokio::test]
    async fn retry_still_rejects_pending_orders() {
        assert_eq!(run(Ok(true), vec![unavailable(), Ok(false)]).await, (false, 2));
    }
    #[tokio::test]
    async fn retry_accepts_only_confirmed_empty_orders() {
        assert_eq!(run(Ok(true), vec![unavailable(), Ok(true)]).await, (true, 2));
    }
}
