use super::{pb, session::Output};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
pub struct RuleConfig {
    pub id: u64,
    pub device_id: String,
    pub name: String,
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
}
impl RuleConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.id > 0 && self.local_port > 0 && self.target_port > 0,
            "端口须在 1–65535 之间"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 40
                && !self.name.chars().any(char::is_control),
            "请输入 1–40 字的映射名称"
        );
        ensure!(
            !self.target_host.is_empty()
                && self.target_host.len() <= 253
                && !self
                    .target_host
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\')),
            "目标地址须为 IP 或域名"
        );
        Ok(())
    }
}
struct Rule {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    streams: Arc<std::sync::Mutex<HashMap<u64, mpsc::Sender<pb::PortMappingFrame>>>>,
}
pub struct Outgoing {
    pub frame: pb::PortMappingFrame,
    pub cancel: CancellationToken,
}
pub struct Ports {
    rules: HashMap<u64, Rule>,
    pub outgoing: mpsc::Receiver<Outgoing>,
    tx: mpsc::Sender<Outgoing>,
    slots: Arc<Semaphore>,
}
impl Default for Ports {
    fn default() -> Self {
        let (tx, outgoing) = mpsc::channel(64);
        Self {
            rules: HashMap::new(),
            outgoing,
            tx,
            slots: Arc::new(Semaphore::new(32)),
        }
    }
}
impl Ports {
    pub async fn enable(&mut self, config: RuleConfig, output: Output) -> Result<()> {
        config.validate()?;
        ensure!(
            self.rules.len() < 32 || self.rules.contains_key(&config.id),
            "最多同时启用 32 条映射"
        );
        ensure!(!self.rules.contains_key(&config.id), "映射已启用");
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.local_port))
            .await
            .context("无法监听本地端口，端口可能已占用或没有权限")?;
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let streams = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let routes = streams.clone();
        let sender = self.tx.clone();
        let slots = self.slots.clone();
        let id = config.id;
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _=stop.cancelled()=>break,
                    Some(_)=tasks.join_next(), if !tasks.is_empty()=>{},
                    accepted=listener.accept()=> {
                        let Ok((socket,_))=accepted else{break;};
                        let Ok(permit)=slots.clone().try_acquire_owned() else {drop(socket);continue;};
                        let stream_id=(uuid::Uuid::new_v4().as_u128() as u64).max(1);
                        let (tx,rx)=mpsc::channel(8);
                        routes.lock().unwrap().insert(stream_id,tx);
                        let routes=routes.clone();let config=config.clone();let output=output.clone();let sender=sender.clone();let cancel=stop.clone();
                        tasks.spawn(async move {
                            let _permit=permit;
                            let result=bridge(socket,rx,&config,stream_id,sender.clone(),cancel.clone(),&output).await;
                            routes.lock().unwrap().remove(&stream_id);
                            let _=tokio::time::timeout(Duration::from_millis(250),sender.send(Outgoing {frame:frame(config.id,stream_id,1,vec![]),cancel})).await;
                            if let Err(e)=result {output.native("port_status",json!({"id":config.id,"enabled":true,"listening":true,"scope":"target","last_target_state":"failed","status":"listening","message":format!("监听中，目标连接失败：{e}")}));}
                        });
                    }
                }
            }
            drop(listener);
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        self.rules.insert(
            id,
            Rule {
                cancel,
                task,
                streams,
            },
        );
        Ok(())
    }
    pub async fn disable(&mut self, id: u64) -> Vec<pb::PortMappingFrame> {
        if let Some(rule) = self.rules.remove(&id) {
            let frames = rule
                .streams
                .lock()
                .unwrap()
                .keys()
                .map(|&stream| frame(id, stream, 1, vec![]))
                .collect();
            rule.cancel.cancel();
            let _ = rule.task.await;
            return frames;
        }
        vec![]
    }
    pub fn receive(&mut self, frame: pb::PortMappingFrame) {
        if frame.payload.len() > 512 * 1024 {
            return;
        }
        let Some(rule) = self.rules.get(&frame.rule_id) else {
            return;
        };
        let mut streams = rule.streams.lock().unwrap();
        if let Some(sender) = streams.get(&frame.stream_id)
            && sender.try_send(frame.clone()).is_err()
        {
            // A stalled local consumer must not grow buffers or stall remote input.
            // Dropping this route makes the stream worker close and send FIN.
            streams.remove(&frame.stream_id);
        }
    }
    pub async fn close(&mut self) -> Vec<pb::PortMappingFrame> {
        let mut frames = vec![];
        for id in self.rules.keys().copied().collect::<Vec<_>>() {
            frames.extend(self.disable(id).await);
        }
        frames
    }
}
fn frame(rule_id: u64, stream_id: u64, r#type: i32, payload: Vec<u8>) -> pb::PortMappingFrame {
    pb::PortMappingFrame {
        session_id: 0,
        rule_id,
        stream_id,
        r#type,
        payload,
    }
}
async fn bridge(
    mut socket: TcpStream,
    mut incoming: mpsc::Receiver<pb::PortMappingFrame>,
    config: &RuleConfig,
    stream_id: u64,
    outgoing: mpsc::Sender<Outgoing>,
    cancel: CancellationToken,
    output: &Output,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let syn = frame(
        config.id,
        stream_id,
        0,
        serde_json::to_vec(
            &json!({"version":1,"target_host":config.target_host,"target_port":config.target_port}),
        )?,
    );
    outgoing
        .send(Outgoing {
            frame: syn,
            cancel: cancel.clone(),
        })
        .await?;
    let ack = tokio::select! {_=cancel.cancelled()=>return Ok(()),ack=tokio::time::timeout(Duration::from_secs(10),incoming.recv())=>ack.context("远端端口连接超时")?.context("映射已关闭")?};
    ensure!(ack.r#type == 3, "远端已拒绝端口连接");
    let ack: serde_json::Value = serde_json::from_slice(&ack.payload)?;
    ensure!(ack["ok"] == true, "远端目标连接失败");
    output.native(
        "port_status",
        json!({"id":config.id,"enabled":true,"listening":true,"scope":"target","last_target_state":"connected","status":"connected","message":"目标已连通"}),
    );
    let mut buffer = vec![0; 32 * 1024];
    loop {
        tokio::select! {
            _=cancel.cancelled()=>return Ok(()),
            result=socket.read(&mut buffer)=> {
                let count=result?;if count==0{return Ok(());}
                tokio::select! {_=cancel.cancelled()=>return Ok(()),result=outgoing.send(Outgoing{frame:frame(config.id,stream_id,2,buffer[..count].to_vec()),cancel:cancel.clone()})=>result?}
            }
            packet=incoming.recv()=> {
                let Some(packet)=packet else{return Ok(());};
                match packet.r#type {
                    1=>{socket.shutdown().await?;return Ok(());}
                    2=> {tokio::time::timeout(Duration::from_secs(5),socket.write_all(&packet.payload)).await.context("本地接收程序长时间无响应")??;}
                    _=>{}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn tunnel_round_trip_and_disable_close_the_listener() {
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_port = reserve.local_addr().unwrap().port();
        drop(reserve);
        let config = RuleConfig {
            id: 7,
            device_id: "test".into(),
            name: "test".into(),
            local_port,
            target_host: "127.0.0.1".into(),
            target_port: 8008,
        };
        let out = Output {
            media: Default::default(),
            generation: Some(1),
            events: std::sync::mpsc::channel().0,
            latest: Default::default(),
            ctx: Default::default(),
        };
        let mut ports = Ports::default();
        ports.enable(config, out).await.unwrap();
        let mut client = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, local_port))
            .await
            .unwrap();
        let syn = tokio::time::timeout(Duration::from_secs(2), ports.outgoing.recv())
            .await
            .unwrap()
            .unwrap()
            .frame;
        assert_eq!(syn.r#type, 0);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&syn.payload).unwrap(),
            json!({"version":1,"target_host":"127.0.0.1","target_port":8008})
        );
        ports.receive(frame(
            7,
            syn.stream_id,
            3,
            br#"{"version":1,"ok":true}"#.to_vec(),
        ));
        client.write_all(b"request").await.unwrap();
        let data = tokio::time::timeout(Duration::from_secs(2), ports.outgoing.recv())
            .await
            .unwrap()
            .unwrap()
            .frame;
        assert_eq!(data.r#type, 2);
        assert_eq!(data.payload, b"request");
        // Incoming data from another rule or stream must never reach this socket.
        ports.receive(frame(8, syn.stream_id, 2, b"wrong".to_vec()));
        ports.receive(frame(
            7,
            syn.stream_id.wrapping_add(1),
            2,
            b"wrong".to_vec(),
        ));
        ports.receive(frame(7, syn.stream_id, 2, b"reply".to_vec()));
        let mut reply = [0; 5];
        tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&reply, b"reply");
        // Reserve the probe's source port while the listener still owns its
        // port. Otherwise Linux can reuse the freed listener port for the
        // connecting socket and establish a TCP self-connection.
        let probe = tokio::net::TcpSocket::new_v4().unwrap();
        probe
            .bind((std::net::Ipv4Addr::LOCALHOST, 0).into())
            .unwrap();
        let closing = ports.disable(7).await;
        assert!(
            closing
                .iter()
                .any(|f| f.r#type == 1 && f.stream_id == syn.stream_id)
        );
        assert!(
            probe
                .connect((std::net::Ipv4Addr::LOCALHOST, local_port).into())
                .await
                .is_err()
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), client.read(&mut reply))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
