#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;

fn fragment(sequence: usize, total: usize, payload: &[u8]) -> Frame {
    Frame {
        method: 1,
        payload: Some(payload.to_vec()),
        headers: vec![
            FrameHeader {
                key: "sum".into(),
                value: total.to_string(),
            },
            FrameHeader {
                key: "seq".into(),
                value: sequence.to_string(),
            },
            FrameHeader {
                key: "message_id".into(),
                value: "event_test".into(),
            },
            FrameHeader {
                key: "type".into(),
                value: "event".into(),
            },
        ],
        ..Default::default()
    }
}

#[test]
fn fragment_assembly_handles_reordering_duplicates_and_rejects_corruption() -> Result<(), Error> {
    let mut assembler = Assembler::default();
    assert_eq!(assembler.combine(&fragment(1, 2, b"world"))?, None);
    assert_eq!(assembler.combine(&fragment(1, 2, b"world"))?, None);
    assert_eq!(
        assembler.combine(&fragment(0, 2, b"hello "))?,
        Some(b"hello world".to_vec())
    );
    assert!(assembler.pending.is_empty());
    assert!(assembler.combine(&fragment(2, 2, b"bad")).is_err());
    assert!(assembler.combine(&fragment(0, 33, b"bad")).is_err());
    assert_eq!(assembler.combine(&fragment(0, 2, b"first"))?, None);
    assert!(assembler.combine(&fragment(0, 2, b"changed")).is_err());
    assert!(assembler.pending.is_empty());
    Ok(())
}

#[test]
fn protobuf_round_trip_preserves_ack_identity() -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = fragment(0, 1, b"{}");
    frame.seq_id = 123;
    frame.log_id = 456;
    frame.service = 7;
    let decoded = Frame::decode(frame.encode_to_vec().as_slice())?;
    assert_eq!(decoded.seq_id, 123);
    assert_eq!(decoded.log_id, 456);
    assert_eq!(frame_header(&decoded, "type")?, "event");
    Ok(())
}

#[tokio::test]
async fn native_socket_bootstraps_fragments_acks_and_reconnects()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (bot, state, http_task) = super::super::tests::fixture(Delivery::Card).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    *state.websocket_url.lock().await = Some(format!("ws://{address}/?service_id=7"));
    let (acks_tx, mut acks_rx) = mpsc::channel(4);
    let socket_task = tokio::spawn(async move {
        for connection in 0..2 {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|_| Error::Invalid("test socket accept"))?;
            let mut socket = rig_tungstenite::tokio_tungstenite::accept_async(stream)
                .await
                .map_err(|_| Error::Invalid("test socket handshake"))?;
            let mut event = super::super::tests::event();
            event["event"]["message"]["message_id"] = json!(format!("om_{connection}"));
            let bytes = serde_json::to_vec(&event)?;
            let midpoint = bytes.len() / 2;
            socket
                .send(Message::Binary(
                    fragment(1, 2, &bytes[midpoint..]).encode_to_vec().into(),
                ))
                .await
                .map_err(|_| Error::Invalid("test fragment send"))?;
            socket
                .send(Message::Binary(
                    fragment(0, 2, &bytes[..midpoint]).encode_to_vec().into(),
                ))
                .await
                .map_err(|_| Error::Invalid("test fragment send"))?;
            loop {
                let message = socket
                    .next()
                    .await
                    .ok_or(Error::Invalid("missing test ack"))?
                    .map_err(|_| Error::Invalid("test ack receive"))?;
                if let Message::Binary(bytes) = message {
                    let frame = Frame::decode(bytes.as_ref())
                        .map_err(|_| Error::Invalid("test protobuf"))?;
                    if frame.method == 1 {
                        let payload = frame.payload.ok_or(Error::Invalid("missing ack body"))?;
                        let value: Value = serde_json::from_slice(&payload)?;
                        acks_tx
                            .send(value)
                            .await
                            .map_err(|_| Error::Invalid("test ack channel"))?;
                        break;
                    }
                }
            }
            socket
                .close(None)
                .await
                .map_err(|_| Error::Invalid("test socket close"))?;
        }
        Ok::<(), Error>(())
    });
    let (events_tx, mut events_rx) = mpsc::channel(4);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client = bot.clone();
    let run = tokio::spawn(async move { client.run_websocket(events_tx, shutdown_rx).await });
    let first = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
        .await?
        .ok_or("first socket event")?;
    assert_eq!(first.inbound.message.message_id, "om_0");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), acks_rx.recv())
            .await?
            .ok_or("first ack")?["code"],
        200
    );
    let second = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
        .await?
        .ok_or("reconnected event")?;
    assert_eq!(second.inbound.message.message_id, "om_1");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), acks_rx.recv())
            .await?
            .ok_or("second ack")?["code"],
        200
    );
    shutdown_tx.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), run).await???;
    socket_task.await??;
    assert_eq!(
        state
            .requests
            .lock()
            .await
            .iter()
            .filter(|(_, path, _)| path == "/callback/ws/endpoint")
            .count(),
        2
    );
    http_task.abort();
    Ok(())
}

#[tokio::test]
async fn socket_endpoint_authentication_rejects_untrusted_hosts()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (bot, _, task) = super::super::tests::fixture(Delivery::Card).await?;
    assert!(
        bot.validate_socket(&Url::parse("wss://attacker.example/?service_id=7")?)
            .is_err()
    );
    assert!(
        bot.validate_socket(&Url::parse(
            "wss://feishu.cn.attacker.example/?service_id=7"
        )?)
        .is_err()
    );
    assert!(
        bot.validate_socket(&Url::parse("wss://msg-frontier.feishu.cn/?service_id=7")?)
            .is_ok()
    );
    task.abort();
    Ok(())
}

#[tokio::test]
async fn silent_peer_triggers_watchdog_and_reconnect()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut bot, state, http_task) = super::super::tests::fixture(Delivery::Card).await?;
    bot.config.timeout = Duration::from_millis(100);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    *state.websocket_url.lock().await =
        Some(format!("ws://{}/?service_id=7", listener.local_addr()?));
    let socket_task = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|_| Error::Invalid("watchdog accept"))?;
        let mut socket = rig_tungstenite::tokio_tungstenite::accept_async(stream)
            .await
            .map_err(|_| Error::Invalid("watchdog handshake"))?;
        while socket.next().await.is_some_and(|message| message.is_ok()) {}
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|_| Error::Invalid("watchdog reconnect accept"))?;
        let mut socket = rig_tungstenite::tokio_tungstenite::accept_async(stream)
            .await
            .map_err(|_| Error::Invalid("watchdog reconnect handshake"))?;
        let bytes = serde_json::to_vec(&super::super::tests::event())?;
        socket
            .send(Message::Binary(
                fragment(0, 1, &bytes).encode_to_vec().into(),
            ))
            .await
            .map_err(|_| Error::Invalid("watchdog event send"))?;
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
        Ok::<(), Error>(())
    });
    let (events_tx, mut events_rx) = mpsc::channel(4);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(async move { bot.run_websocket(events_tx, shutdown_rx).await });
    let event = tokio::time::timeout(Duration::from_secs(6), events_rx.recv())
        .await?
        .ok_or("missing watchdog reconnect event")?;
    assert_eq!(event.inbound.message.message_id, "om_input");
    shutdown_tx.send(true)?;
    tokio::time::timeout(Duration::from_secs(2), run).await???;
    tokio::time::timeout(Duration::from_secs(2), socket_task).await???;
    assert!(
        state
            .requests
            .lock()
            .await
            .iter()
            .filter(|(_, path, _)| path == "/callback/ws/endpoint")
            .count()
            >= 2
    );
    http_task.abort();
    Ok(())
}
