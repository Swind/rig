use rig_messaging_platforms::Error;
use std::{future::Future, time::Duration};
use tokio::{sync::watch, task::JoinHandle};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) async fn supervise<S, C>(
    server: S,
    mut worker: Option<JoinHandle<std::result::Result<(), Error>>>,
    shutdown: watch::Sender<bool>,
    signal: C,
) -> Result<()>
where
    S: Future<Output = std::io::Result<()>>,
    C: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(server);
    let native = async {
        match worker.as_mut() {
            Some(worker) => worker.await,
            None => std::future::pending().await,
        }
    };
    let (result, server_done, worker_done): (Result<()>, bool, bool) = tokio::select! {
        result=&mut server=>(result.map_err(Into::into),true,false),
        result=native=>(match result {
            Ok(Err(error))=>Err(error.into()),
            Ok(Ok(()))=>Err(std::io::Error::other("native messaging transport stopped").into()),
            Err(error)=>Err(error.into()),
        },false,true),
        result=signal=>(result.map_err(Into::into),false,false),
    };
    shutdown.send_replace(true);
    let drain = async {
        tokio::try_join!(
            async {
                if !server_done {
                    server.await?;
                }
                Ok::<(), Box<dyn std::error::Error>>(())
            },
            async {
                if !worker_done && let Some(worker) = worker.as_mut() {
                    worker.await??;
                }
                Ok::<(), Box<dyn std::error::Error>>(())
            }
        )
        .map(|_| ())
    };
    match tokio::time::timeout(Duration::from_secs(20), drain).await {
        Ok(drained) => drained?,
        Err(_) => {
            if let Some(worker) = worker {
                worker.abort();
            }
            return Err(std::io::Error::other("messaging shutdown timed out").into());
        }
    }
    result
}
#[cfg(test)]
mod tests;
