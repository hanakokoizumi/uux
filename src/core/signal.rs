use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

pub struct Event {
    pub name: String,
    pub args: Vec<Value>,
    pub binary: Vec<Vec<u8>>,
}

const MAX_APP_DATA: usize = 2 * 1024 * 1024;

/// Official forward_setting envelope (streamer 4.33: 180a8ae90,
/// SignalAppData parser 180a89040). Socket.IO owns the binary attachment.
pub fn app_data_envelope(remote: &str, ice: &str) -> Value {
    json!({"client_id":remote,"data":{
        "type":"signal_app_data","signal_app_data":{
            "ice_id":ice,"binary_data":{"_placeholder":true,"num":0}
        }
    }})
}

/// Bind forwarded application data to this peer and this connection. Never
/// consume an arbitrary attachment, a stale ICE session, or another sender.
pub fn forwarded_app_data<'a>(event: &'a Event, remote: &str, ice: &str) -> Option<&'a [u8]> {
    if event.name != "forward_setting" || remote.is_empty() || ice.is_empty() {
        return None;
    }
    let arg = event.args.first()?;
    let data = &arg["data"];
    let app = &data["signal_app_data"];
    if arg["client_id"] != remote || data["type"] != "signal_app_data" || app["ice_id"] != ice {
        return None;
    }
    let placeholder = &app["binary_data"];
    if placeholder["_placeholder"] != true {
        return None;
    }
    let index = usize::try_from(placeholder["num"].as_u64()?).ok()?;
    event
        .binary
        .get(index)
        .map(Vec::as_slice)
        .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_APP_DATA)
}

struct Packet {
    kind: u8,
    id: Option<u64>,
    value: Value,
    attachments: usize,
}
fn decode(text: &str) -> Result<Packet> {
    let kind = text
        .as_bytes()
        .first()
        .copied()
        .context("空信令")?
        .wrapping_sub(b'0');
    ensure!(kind <= 6, "信令类型无效");
    let mut rest = &text[1..];
    let mut attachments = 0;
    if kind == 5 || kind == 6 {
        let (n, r) = rest.split_once('-').context("附件格式错误")?;
        attachments = n.parse::<usize>()?;
        ensure!((1..=32).contains(&attachments), "附件数量无效");
        rest = r;
    }
    if rest.starts_with('/') {
        bail!("不支持的信令命名空间");
    }
    let count = rest.bytes().take_while(u8::is_ascii_digit).count();
    let id = if count > 0 {
        Some(rest[..count].parse()?)
    } else {
        None
    };
    let value = if rest[count..].is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&rest[count..])?
    };
    Ok(Packet {
        kind,
        id,
        value,
        attachments,
    })
}

type Socket = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type Ack = (Value, Vec<Vec<u8>>);

#[derive(Clone)]
pub struct Signal {
    socket: Arc<Mutex<Socket>>,
    acks: Arc<Mutex<HashMap<u64, oneshot::Sender<Ack>>>>,
    next: Arc<AtomicU64>,
    cancel: tokio_util::sync::CancellationToken,
    ended: tokio_util::sync::CancellationToken,
}

impl Signal {
    pub async fn connect(base: &str, token: &str) -> Result<(Self, mpsc::Receiver<Result<Event>>)> {
        let mut url = url::Url::parse(base)?;
        let scheme = match url.scheme() {
            "https" | "wss" => "wss",
            "http" | "ws" => "ws",
            _ => bail!("信令地址无效"),
        };
        url.set_scheme(scheme).ok();
        let path = url.path().trim_end_matches('/');
        if !path.ends_with("/socket.io") {
            url.set_path(&format!("{path}/socket.io/"));
        } else {
            url.set_path(&format!("{path}/"));
        }
        url.set_query(Some("EIO=4&transport=websocket"));
        let mut request = url.as_str().into_client_request()?;
        for (k, v) in [
            ("x-nrd-auth", token),
            ("x-nrd-controlling", "0"),
            ("streamer_version", "V4.3.0"),
            ("streamer_flag", "1"),
        ] {
            request.headers_mut().insert(k, v.parse()?);
        }
        let (mut ws, _) = tokio::time::timeout(Duration::from_secs(15), connect_async(request))
            .await?
            .map_err(|_| anyhow::anyhow!("无法建立信令连接"))?;
        let first = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await?
            .context("信令提前断开")??;
        let text = first.to_text()?;
        ensure!(text.starts_with('0'), "信令握手无效");
        let hello: Value = serde_json::from_str(&text[1..])?;
        let heartbeat = hello["pingInterval"].as_u64().unwrap_or(25000)
            + hello["pingTimeout"].as_u64().unwrap_or(20000);
        ws.send(Message::Text("40".into())).await?;
        let (socket, mut reader) = ws.split();
        let this = Self {
            socket: Arc::new(Mutex::new(socket)),
            acks: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicU64::new(0)),
            cancel: Default::default(),
            ended: Default::default(),
        };
        let guard = this.cancel.clone().drop_guard();
        let (tx, rx) = mpsc::channel(128);
        let (ready_tx, ready_rx) = oneshot::channel();
        let signal = this.clone();
        tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let mut pending: Option<Packet> = None;
            let mut binaries = vec![];
            let result: Result<()> = tokio::select! {_=signal.cancel.cancelled()=>Ok(()), result=async {
                loop {
                    let frame=tokio::time::timeout(Duration::from_millis(heartbeat.min(120000)),reader.next()).await?
                        .context("信令已断开")??;
                    let packet=match frame {
                        Message::Binary(data)=>{
                            ensure!(pending.is_some() && data.first()==Some(&4),"信令附件格式无效");
                            binaries.push(data[1..].to_vec());
                            if binaries.len()!=pending.as_ref().unwrap().attachments {continue;}
                            pending.take().unwrap()
                        },
                        Message::Text(text)=>{
                            if let Some(ping)=text.strip_prefix('2') {
                                signal.socket.lock().await.send(Message::Text(format!("3{ping}").into())).await?;
                                continue;
                            }
                            if text.starts_with('1') {bail!("服务端关闭了信令连接");}
                            if text=="40" || text.starts_with("40{") {
                                if let Some(tx)=ready.take(){let _=tx.send(());}
                                continue;
                            }
                            let Some(text)=text.strip_prefix('4') else {continue};
                            let packet=decode(text)?;
                            if packet.attachments>0 {
                                ensure!(pending.is_none(),"信令附件交错");
                                pending=Some(packet);continue;
                            }
                            packet
                        },
                        Message::Close(_)=>bail!("信令已断开"),
                        _=>continue,
                    };
                    let data=std::mem::take(&mut binaries);
                    match packet.kind {
                        3|6=>{
                            if let Some(id)=packet.id && let Some(tx)=signal.acks.lock().await.remove(&id) {
                                let _=tx.send((packet.value,data));
                            }
                        },
                        2|5=>{
                            let values=packet.value.as_array().context("信令事件无效")?;
                            let name=values.first().and_then(Value::as_str).context("信令事件缺少名称")?.to_owned();
                            if let Some(id)=packet.id {
                                signal.socket.lock().await.send(Message::Text(format!("43{id}[]").into())).await?;
                            }
                            tx.send(Ok(Event{name,args:values[1..].to_vec(),binary:data})).await
                                .map_err(|_|anyhow::anyhow!("事件接收已关闭"))?;
                        },
                        4=>bail!("服务端拒绝信令连接"),
                        _=>{},
                    }
                }
            }=>result};
            signal.acks.lock().await.clear();
            signal.ended.cancel();
            if let Err(e) = result {
                let _ = tx.send(Err(e)).await;
            }
        });
        tokio::time::timeout(Duration::from_secs(10), ready_rx).await??;
        guard.disarm();
        Ok((this, rx))
    }

    pub async fn emit(
        &self,
        name: &str,
        args: Vec<Value>,
        binary: Vec<Vec<u8>>,
        ack: bool,
    ) -> Result<Ack> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if ack {
            self.acks.lock().await.insert(id, tx);
        }
        let mut data = vec![json!(name)];
        data.extend(args);
        let kind = if binary.is_empty() {
            "42".to_string()
        } else {
            format!("45{}-", binary.len())
        };
        let text = format!(
            "{kind}{}{}",
            if ack { id.to_string() } else { String::new() },
            serde_json::to_string(&data)?
        );
        {
            let mut socket = self.socket.lock().await;
            socket.send(Message::Text(text.into())).await?;
            for mut bytes in binary {
                bytes.insert(0, 4);
                socket.send(Message::Binary(bytes.into())).await?;
            }
        }
        if !ack {
            return Ok((Value::Null, vec![]));
        }
        let result = tokio::time::timeout(Duration::from_secs(10), rx).await;
        self.acks.lock().await.remove(&id);
        result.context("等待信令确认超时")?.context("信令已关闭")
    }

    pub async fn close(&self, ice: Option<&str>) -> bool {
        // The native release event has no ACK callback. Queue release and leave
        // in order, then complete the WebSocket close handshake within a bound.
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            if let Some(ice) = ice {
                self.emit(
                    "release",
                    vec![json!({"ice_id":ice,"data":{}})],
                    vec![],
                    false,
                )
                .await?;
            }
            self.emit("leave", vec![], vec![], false).await?;
            let mut socket = self.socket.lock().await;
            socket.send(Message::Text("41".into())).await?;
            socket.send(Message::Text("1".into())).await?;
            socket.close().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        // Give the peer a chance to process the queued release and close reply.
        let _ = tokio::time::timeout(Duration::from_millis(500), self.ended.cancelled()).await;
        self.cancel.cancel();
        self.acks.lock().await.clear();
        result.is_ok_and(|v| v.is_ok())
    }
    pub fn is_closed(&self) -> bool {
        self.ended.is_cancelled()
    }
}

pub fn sdp(event: &Event) -> Result<String> {
    let data = &event.args[0]["data"];
    if let Some(index) = data["gzip_sdp"]["num"].as_u64() {
        let bytes = event.binary.get(index as usize).context("缺少 SDP 附件")?;
        let mut output = String::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .take(1024 * 1024 + 1)
            .read_to_string(&mut output)?;
        ensure!(output.len() <= 1024 * 1024, "SDP 过大");
        Ok(output)
    } else {
        Ok(data["sdp"].as_str().context("缺少 SDP")?.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarded_binary_is_bound_to_peer_session_and_placeholder() {
        let mut event = Event {
            name: "forward_setting".into(),
            args: vec![app_data_envelope("peer", "session")],
            binary: vec![vec![1, 2, 3]],
        };
        assert_eq!(
            forwarded_app_data(&event, "peer", "session"),
            Some(&[1, 2, 3][..])
        );
        assert!(forwarded_app_data(&event, "another", "session").is_none());
        assert!(forwarded_app_data(&event, "peer", "stale").is_none());
        assert!(forwarded_app_data(&event, "", "session").is_none());
        for index in [
            json!(-1),
            json!(1),
            json!("0"),
            Value::Null,
            json!(u64::MAX),
        ] {
            event.args[0]["data"]["signal_app_data"]["binary_data"]["num"] = index;
            assert!(forwarded_app_data(&event, "peer", "session").is_none());
        }
        event.args[0] = app_data_envelope("peer", "session");
        event.args[0]["data"]["signal_app_data"]["binary_data"]["_placeholder"] = json!(false);
        assert!(forwarded_app_data(&event, "peer", "session").is_none());
        event.args[0] = app_data_envelope("peer", "session");
        event.binary[0].resize(MAX_APP_DATA + 1, 0);
        assert!(forwarded_app_data(&event, "peer", "session").is_none());
        event.binary[0].clear();
        assert!(forwarded_app_data(&event, "peer", "session").is_none());
        event.binary[0].push(1);
        event.name = "soac".into();
        assert!(forwarded_app_data(&event, "peer", "session").is_none());
    }
    #[test]
    fn socket_binary_ack_framing() {
        let packet =
            decode("61-23[\"success\",{\"app_data\":{\"_placeholder\":true,\"num\":0}}]").unwrap();
        assert_eq!(
            (packet.kind, packet.id, packet.attachments),
            (6, Some(23), 1)
        );
        assert!(decode("5999-[]").is_err());
    }
}
