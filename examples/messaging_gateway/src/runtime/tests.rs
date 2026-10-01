#![allow(clippy::panic_in_result_fn)]
use super::*;
#[tokio::test]
async fn fatal_native_error_stops_server_and_returns_failure() -> Result<()> {
    let (shutdown, mut stopped) = watch::channel(false);
    let server = async move {
        let _ = stopped.changed().await;
        Ok(())
    };
    let worker = tokio::spawn(async { Err(Error::Authentication) });
    let outcome = supervise(server, Some(worker), shutdown, std::future::pending()).await;
    assert!(outcome.is_err());
    Ok(())
}
#[tokio::test]
async fn signal_closes_server_and_native_worker() -> Result<()> {
    let (shutdown, mut stopped) = watch::channel(false);
    let mut native_stopped = shutdown.subscribe();
    let server = async move {
        let _ = stopped.changed().await;
        Ok(())
    };
    let worker = tokio::spawn(async move {
        let _ = native_stopped.changed().await;
        Ok(())
    });
    supervise(server, Some(worker), shutdown, async { Ok(()) }).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn stalled_server_and_worker_cannot_block_shutdown() -> Result<()> {
    let (shutdown, _) = watch::channel(false);
    let worker = tokio::spawn(std::future::pending());
    let started = tokio::time::Instant::now();
    let outcome = supervise(std::future::pending(), Some(worker), shutdown, async {
        Ok(())
    })
    .await;
    assert!(outcome.is_err());
    assert_eq!(started.elapsed(), Duration::from_secs(20));
    Ok(())
}
