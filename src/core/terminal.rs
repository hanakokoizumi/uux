//! UU TERM frames carried directly on BINARY_DATA_CHANNEL (no local IPC prefix).
use super::session::Output;
use anyhow::{Result, ensure};
use base64::Engine;
use serde_json::{Value, json};

const MAX_PAYLOAD: usize = 1024 * 1024;
pub fn encode(kind: u8, session: u32, payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(payload.len() <= MAX_PAYLOAD, "终端数据过大");
    let mut out = Vec::with_capacity(16 + payload.len());
    out.extend_from_slice(b"TERM");
    out.extend_from_slice(&[1, kind, 0, 0]);
    out.extend_from_slice(&session.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}
fn decode(bytes: &[u8]) -> Result<(u8, u32, &[u8])> {
    ensure!(
        bytes.len() >= 16 && &bytes[..5] == b"TERM\x01",
        "终端帧头无效"
    );
    let size = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    ensure!(
        size <= MAX_PAYLOAD && bytes.len() == 16 + size,
        "终端帧长度无效"
    );
    ensure!(bytes[6..8] == [0, 0], "不支持的终端帧标志");
    Ok((
        bytes[5],
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        &bytes[16..],
    ))
}
#[derive(Default)]
pub struct Terminal {
    id: Option<u32>,
    closing: Option<u32>,
}
impl Terminal {
    pub fn command(&mut self, value: &Value) -> Result<Vec<u8>> {
        let op = value["action"].as_str().unwrap_or("");
        if op == "verify_closed" {
            return encode(
                9,
                self.closing
                    .ok_or_else(|| anyhow::anyhow!("没有等待关闭的终端"))?,
                &[],
            );
        }
        if op == "open" {
            ensure!(self.id.is_none(), "终端已打开");
            let shell = value["shell"].as_str().unwrap_or("powershell");
            ensure!(
                ["powershell", "cmd", "zsh", "bash"].contains(&shell),
                "不支持的终端类型"
            );
            let id = (uuid::Uuid::new_v4().as_u128() as u32).max(1);
            let payload = serde_json::to_vec(
                &json!({"shell":shell,"cols":value["cols"].as_u64().unwrap_or(100).clamp(20,400),
                "rows":value["rows"].as_u64().unwrap_or(30).clamp(5,200),"option":"verify"}),
            )?;
            let result = encode(1, id, &payload)?;
            self.id = Some(id);
            return Ok(result);
        }
        let id = self.id.ok_or_else(|| anyhow::anyhow!("终端尚未打开"))?;
        let (kind, payload) = match op {
            "input" => (
                2,
                base64::engine::general_purpose::STANDARD
                    .decode(value["data"].as_str().unwrap_or(""))?,
            ),
            "resize" => (
                3,
                serde_json::to_vec(
                    &json!({"cols":value["cols"].as_u64().unwrap_or(100).clamp(20,400),"rows":value["rows"].as_u64().unwrap_or(30).clamp(5,200)}),
                )?,
            ),
            "interrupt" => (7, serde_json::to_vec(&json!({"sig":"SIGINT"}))?),
            "close" => {
                self.id = None;
                self.closing = Some(id);
                (4, vec![])
            }
            _ => anyhow::bail!("不支持的终端操作"),
        };
        encode(kind, id, &payload)
    }
    pub fn receive(&mut self, bytes: &[u8], out: &Output) -> Result<()> {
        let (kind, id, payload) = decode(bytes)?;
        if kind == 10 && Some(id) == self.closing {
            let data: Value = serde_json::from_slice(payload)?;
            // The host serializes the session array itself, without a
            // surrounding object (4.40 server 1406728a0).
            let sessions = data
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("终端列表格式无效"))?;
            ensure!(
                sessions.iter().all(|s| s["session_id"].as_u64().is_some()),
                "终端列表缺少会话 ID"
            );
            if !sessions
                .iter()
                .any(|s| s["session_id"].as_u64() == Some(u64::from(id)))
            {
                self.closing = None;
                out.native("terminal_closed", json!({"verified_absent":true}));
            }
            return Ok(());
        }
        let active = Some(id) == self.id;
        if !active && !(Some(id) == self.closing && kind == 6) {
            return Ok(());
        }
        if !active {
            let data: Value = serde_json::from_slice(payload)?;
            self.closing = None;
            out.native("terminal_closed", json!({"exit_code":data["exit_code"]}));
            return Ok(());
        }
        if kind == 5 {
            out.native(
                "terminal_output",
                json!({"data":base64::engine::general_purpose::STANDARD.encode(payload)}),
            );
        } else if matches!(kind, 6 | 8 | 13 | 15) {
            let data: Value = serde_json::from_slice(payload)?;
            out.native("terminal_status", json!({"kind":kind,"result":data}));
            if kind == 6 {
                if active {
                    self.id = None;
                }
                if Some(id) == self.closing {
                    self.closing = None;
                }
            }
        }
        Ok(())
    }
    pub fn close(&mut self) -> Option<Vec<u8>> {
        let id = self.id.take()?;
        self.closing = Some(id);
        // Close only the shell created by this window; detach (16) would leave
        // an inaccessible background shell because we do not manage saved sessions.
        encode(4, id, &[]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_verification_requires_a_valid_list_without_our_session() {
        let mut terminal = Terminal::default();
        let open = terminal.command(&json!({"action":"open"})).unwrap();
        let id = decode(&open).unwrap().1;
        terminal.close().unwrap();
        let verify = terminal
            .command(&json!({"action":"verify_closed"}))
            .unwrap();
        assert_eq!(decode(&verify).unwrap(), (9, id, &[][..]));
        let output = Output {
            media: Default::default(),
            generation: Some(1),
            events: std::sync::mpsc::channel().0,
            latest: Default::default(),
            ctx: Default::default(),
        };
        let present = serde_json::to_vec(&json!([{"session_id":id}])).unwrap();
        terminal
            .receive(&encode(10, id, &present).unwrap(), &output)
            .unwrap();
        assert_eq!(terminal.closing, Some(id));
        assert!(
            terminal
                .receive(&encode(10, id, b"[{}]").unwrap(), &output)
                .is_err()
        );
        assert_eq!(terminal.closing, Some(id));
        terminal
            .receive(&encode(10, id, b"[]").unwrap(), &output)
            .unwrap();
        assert_eq!(terminal.closing, None);
    }
    #[test]
    fn close_targets_own_shell_and_late_exit_does_not_close_reopened_terminal() {
        let mut terminal = Terminal::default();
        let first = terminal.command(&json!({"action":"open"})).unwrap();
        let (_, first_id, payload) = decode(&first).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(payload).unwrap()["option"],
            "verify"
        );
        let close = terminal.close().unwrap();
        assert_eq!(decode(&close).unwrap(), (4, first_id, &[][..]));
        let reopened = terminal.command(&json!({"action":"open"})).unwrap();
        let second_id = decode(&reopened).unwrap().1;
        let output = Output {
            media: Default::default(),
            generation: Some(1),
            events: std::sync::mpsc::channel().0,
            latest: Default::default(),
            ctx: Default::default(),
        };
        terminal
            .receive(
                &encode(6, first_id, br#"{"exit_code":0}"#).unwrap(),
                &output,
            )
            .unwrap();
        assert_eq!(terminal.id, Some(second_id));
    }
    #[test]
    fn frame_matches_official_serializer_and_rejects_truncated_or_extra_data() {
        let frame = encode(2, 0x12345678, b"abc").unwrap();
        assert_eq!(frame, b"TERM\x01\x02\0\0\x78\x56\x34\x12\x03\0\0\0abc");
        assert_eq!(decode(&frame).unwrap(), (2, 0x12345678, b"abc".as_slice()));
        for end in 0..frame.len() {
            assert!(decode(&frame[..end]).is_err());
        }
        let mut bad = frame;
        bad.push(0);
        assert!(decode(&bad).is_err());
    }
}
