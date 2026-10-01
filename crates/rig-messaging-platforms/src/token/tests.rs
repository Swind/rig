#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[tokio::test(start_paused = true)]
async fn refresh_is_serialized_and_expiry_is_reserved() -> Result<(), Error> {
    let cache = TokenCache::new();
    let calls = AtomicUsize::new(0);
    let refresh = || async {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(("token".to_owned(), Duration::from_secs(100)))
    };
    let (first, second) =
        tokio::join!(cache.get_or_refresh(refresh), cache.get_or_refresh(refresh));
    assert_eq!(first?, second?);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    tokio::time::advance(Duration::from_secs(91)).await;
    cache.get_or_refresh(refresh).await?;
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    cache.invalidate().await;
    cache.get_or_refresh(refresh).await?;
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    Ok(())
}
