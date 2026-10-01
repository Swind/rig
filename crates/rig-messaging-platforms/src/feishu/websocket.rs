use super::*;
use futures::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use rig_reqwest::reqwest::Url;
use rig_tungstenite::tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use std::time::Instant;
use tokio::sync::{mpsc, watch};

/// Feishu protobuf long-connection frame.
#[derive(Clone, PartialEq, ProstMessage)]
pub struct Frame {
    /// Protocol sequence identifier, echoed in acknowledgements.
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    /// Protocol log identifier.
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    /// Service identifier obtained from the authenticated endpoint.
    #[prost(int32, tag = "3")]
    pub service: i32,
    /// Zero for control frames, one for data frames.
    #[prost(int32, tag = "4")]
    pub method: i32,
    /// Protocol metadata, including event type and fragment sequence.
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<FrameHeader>,
    /// Payload encoding.
    #[prost(string, optional, tag = "6")]
    pub payload_encoding: Option<String>,
    /// Payload MIME type.
    #[prost(string, optional, tag = "7")]
    pub payload_type: Option<String>,
    /// Event or control payload.
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
    /// New protocol log identifier.
    #[prost(string, optional, tag = "9")]
    pub log_id_new: Option<String>,
}

/// Header on a Feishu protobuf frame.
#[derive(Clone, PartialEq, ProstMessage)]
pub struct FrameHeader {
    /// Protocol header name.
    #[prost(string, tag = "1")]
    pub key: String,
    /// Protocol header value.
    #[prost(string, tag = "2")]
    pub value: String,
}

struct Fragments {
    chunks: Vec<Option<Vec<u8>>>,
    bytes: usize,
    since: Instant,
}

#[derive(Default)]
struct Assembler {
    pending: HashMap<String, Fragments>,
}

impl Assembler {
    fn combine(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>, Error> {
        self.pending
            .retain(|_, set| set.since.elapsed() < Duration::from_secs(5));
        let total = frame_header(frame, "sum")?
            .parse::<usize>()
            .map_err(|_| Error::Invalid("invalid fragment count"))?;
        let sequence = frame_header(frame, "seq")?
            .parse::<usize>()
            .map_err(|_| Error::Invalid("invalid fragment sequence"))?;
        if total == 0 || total > 32 || sequence >= total {
            return Err(Error::Invalid("invalid Feishu fragment sequence"));
        }
        let payload = frame
            .payload
            .as_ref()
            .ok_or(Error::Invalid("missing websocket payload"))?;
        if payload.len() > 1024 * 1024 {
            return Err(Error::TooLarge);
        }
        if total == 1 {
            return Ok(Some(payload.clone()));
        }
        if self.pending.values().map(|set| set.bytes).sum::<usize>() + payload.len()
            > 8 * 1024 * 1024
        {
            return Err(Error::TooLarge);
        }
        let id = frame_header(frame, "message_id")?;
        if id.len() > 512 {
            return Err(Error::TooLarge);
        }
        if !self.pending.contains_key(id) && self.pending.len() >= 128 {
            return Err(Error::TooLarge);
        }
        let set = self.pending.entry(id.into()).or_insert_with(|| Fragments {
            chunks: vec![None; total],
            bytes: 0,
            since: Instant::now(),
        });
        if set.chunks.len() != total {
            self.pending.remove(id);
            return Err(Error::Invalid("fragment count changed"));
        }
        let slot = set
            .chunks
            .get_mut(sequence)
            .ok_or(Error::Invalid("fragment sequence"))?;
        if let Some(previous) = slot {
            if previous != payload {
                self.pending.remove(id);
                return Err(Error::Invalid("fragment changed"));
            }
        } else {
            if set.bytes + payload.len() > 1024 * 1024 {
                self.pending.remove(id);
                return Err(Error::TooLarge);
            }
            set.bytes += payload.len();
            *slot = Some(payload.clone());
        }
        if set.chunks.iter().any(Option::is_none) {
            return Ok(None);
        }
        let set = self
            .pending
            .remove(id)
            .ok_or(Error::Invalid("fragment assembly missing"))?;
        let mut bytes = Vec::with_capacity(set.bytes);
        for chunk in set.chunks.into_iter().flatten() {
            bytes.extend(chunk);
        }
        Ok(Some(bytes))
    }
}

impl Feishu {
    /// Receive events over an authenticated outbound long connection until shutdown.
    /// Feed the bounded event receiver through Gateway admission before calling prepare.
    /// A full queue returns a failed event acknowledgement, allowing platform retries.
    pub async fn run_websocket(
        &self,
        events: mpsc::Sender<Incoming>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        let mut delay = Duration::from_secs(1);
        loop {
            if *shutdown.borrow() || events.is_closed() {
                return Ok(());
            }
            let result = tokio::select! {
                result=self.websocket_once(&events,&mut shutdown)=>result,
                _=events.closed()=>return Ok(()),
            };
            if *shutdown.borrow() || events.is_closed() {
                return Ok(());
            }
            if matches!(result, Err(Error::Authentication)) {
                return result;
            }
            if result.is_ok() {
                delay = Duration::from_secs(1);
            }
            tokio::select! {
                _=tokio::time::sleep(delay)=>{},
                _=shutdown.changed()=>return Ok(()),
                _=events.closed()=>return Ok(()),
            }
            delay = (delay * 2).min(Duration::from_secs(120));
        }
    }

    async fn websocket_once(
        &self,
        events: &mpsc::Sender<Incoming>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), Error> {
        let body = json!({"AppID":self.config.app_id,"AppSecret":self.config.app_secret});
        let value: Value = self
            .http
            .json(
                self.http
                    .request(Method::POST, &format!("{}/callback/ws/endpoint", self.base))
                    .json(&body),
            )
            .await
            .map_err(|error| match error {
                Error::Status(http::StatusCode::UNAUTHORIZED | http::StatusCode::FORBIDDEN) => {
                    Error::Authentication
                }
                error => error,
            })?;
        if matches!(
            value
                .pointer("/code")
                .unwrap_or(&serde_json::Value::Null)
                .as_i64(),
            Some(403 | 514)
        ) {
            return Err(Error::Authentication);
        }
        check(&value)?;
        let url = Url::parse(required(&value, "/data/URL")?)
            .map_err(|_| Error::Invalid("invalid Feishu websocket endpoint"))?;
        self.validate_socket(&url)?;
        let service = url
            .query_pairs()
            .find(|(key, _)| key == "service_id")
            .and_then(|(_, v)| v.parse::<i32>().ok())
            .ok_or(Error::Invalid("missing websocket service ID"))?;
        let seconds = value
            .pointer("/data/ClientConfig/PingInterval")
            .and_then(Value::as_u64)
            .unwrap_or(120)
            .clamp(1, 300);
        let options = WebSocketConfig::default()
            .max_message_size(Some(1024 * 1024))
            .max_frame_size(Some(1024 * 1024));
        let connection = tokio::time::timeout(
            self.config.timeout,
            connect_async_with_config(url.as_str(), Some(options), false),
        );
        let (mut socket, _) = tokio::select! {
            result=connection=>result.map_err(|_|Error::Invalid("Feishu websocket handshake timed out"))?
                .map_err(|_|Error::Invalid("Feishu websocket handshake failed"))?,
            _=shutdown.changed()=>return Ok(()),
        };
        let mut ping = tokio::time::interval(Duration::from_secs(seconds));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_received = tokio::time::Instant::now();
        let mut idle_limit = Duration::from_secs(seconds) * 2 + self.config.timeout;
        let mut fragments = Assembler::default();
        loop {
            tokio::select! {
                _=shutdown.changed()=>{
                    let _=tokio::time::timeout(self.config.timeout,socket.close(None)).await;
                    return Ok(());
                },
                _=ping.tick()=>{
                    let frame = Frame {service,method:0,headers:vec![FrameHeader {key:"type".into(),value:"ping".into()}],..Default::default()};
                    socket_send(&mut socket,Message::Binary(frame.encode_to_vec().into()),self.config.timeout).await?;
                },
                _=tokio::time::sleep_until(last_received+idle_limit)=>return Err(Error::Invalid("Feishu websocket peer stopped responding")),
                message=socket.next()=>{
                    last_received=tokio::time::Instant::now();
                    match message {
                        Some(Ok(Message::Binary(bytes)))=>{
                            let mut frame = Frame::decode(bytes.as_ref()).map_err(|_|Error::Invalid("invalid Feishu protobuf frame"))?;
                            if frame.method==0 {
                                if frame_header(&frame,"type").ok()==Some("pong")
                                    && let Some(payload)=&frame.payload {
                                        let config:Value=serde_json::from_slice(payload)?;
                                        if let Some(seconds)=config.pointer("/PingInterval").unwrap_or(&serde_json::Value::Null).as_u64() {
                                            ping=tokio::time::interval(Duration::from_secs(seconds.clamp(1,300)));
                                            ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                                            idle_limit=Duration::from_secs(seconds.clamp(1,300))*2+self.config.timeout;
                                        }
                                }
                                continue;
                            }
                            if frame.method!=1 {return Err(Error::Invalid("unknown Feishu frame method"));}
                            let Some(payload)=fragments.combine(&frame)? else {continue;};
                            let normalized = if frame_header(&frame,"type")?=="event" {
                                let value:Result<Value,_>=serde_json::from_slice(&payload);
                                value.map_err(Error::from).and_then(|value|self.normalize(&value))
                            } else {Ok(None)};
                            let permit=if matches!(&normalized,Ok(Some(_))) {events.try_reserve().ok()} else {None};
                            let code=match &normalized {
                                Ok(Some(_)) if permit.is_some()=>200,
                                Ok(None)=>200,
                                _=>500,
                            };
                            frame.payload=Some(serde_json::to_vec(&json!({"code":code}))?);
                            socket_send(&mut socket,Message::Binary(frame.encode_to_vec().into()),self.config.timeout).await?;
                            if let (Some(permit),Ok(Some(event)))=(permit,normalized) {permit.send(event);}
                        },
                        Some(Ok(Message::Ping(bytes)))=>socket_send(&mut socket,Message::Pong(bytes),self.config.timeout).await?,
                        Some(Ok(Message::Close(_)))|None=>return Ok(()),
                        Some(Err(_))=>return Err(Error::Invalid("Feishu websocket receive failed")),
                        _=>{},
                    }
                },
            }
        }
    }

    fn validate_socket(&self, url: &Url) -> Result<(), Error> {
        let suffix = match self.config.domain {
            Domain::Feishu => "feishu.cn",
            Domain::Lark => "larksuite.com",
        };
        let trusted = url.scheme() == "wss"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none_or(|p| p == 443)
            && url
                .host_str()
                .is_some_and(|host| host == suffix || host.ends_with(&format!(".{suffix}")));
        if trusted {
            return Ok(());
        }
        #[cfg(test)]
        if self.base.starts_with("http://127.0.0.1:")
            && url.scheme() == "ws"
            && url.host_str() == Some("127.0.0.1")
        {
            return Ok(());
        }
        Err(Error::Authentication)
    }
}

async fn socket_send<S>(socket: &mut S, message: Message, timeout: Duration) -> Result<(), Error>
where
    S: futures::Sink<Message> + Unpin,
{
    tokio::time::timeout(timeout, socket.send(message))
        .await
        .map_err(|_| Error::Invalid("Feishu websocket send timed out"))?
        .map_err(|_| Error::Invalid("Feishu websocket send failed"))
}

fn frame_header<'a>(frame: &'a Frame, key: &str) -> Result<&'a str, Error> {
    frame
        .headers
        .iter()
        .find(|header| header.key == key)
        .map(|header| header.value.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(Error::Invalid("missing Feishu frame header"))
}

#[cfg(test)]
mod tests;
