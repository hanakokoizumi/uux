//! Windows CF_UNICODETEXT is UTF-16LE on the block transport. The newer
//! TextChange RPC uses a protobuf UTF-8 string instead.
use super::{pb, wire};
use anyhow::{Result, ensure};
use pb::{clipboard_request::Which as Req, clipboard_response::Which as Rsp};
use std::collections::{BTreeMap, HashMap};
pub fn request_summary(r: &pb::ClipboardRequest) -> serde_json::Value {
    use serde_json::json;
    match &r.which {
        Some(Req::FormatList(r)) => {
            json!({"kind":"formats","ids":r.formats.iter().take(32).map(|f|f.id).collect::<Vec<_>>(),"action":r.has_action})
        }
        Some(Req::FormatDataAsk(r)) => json!({"kind":"ask","format":r.format_id}),
        Some(Req::DataBlock(r)) => json!({"kind":"block","block":r.block_id,"bytes":r.data.len()}),
        Some(Req::FileDescListRequest(r)) => json!({"kind":"file_list","task":r.task_id}),
        Some(Req::FileContentsRequest(r)) => {
            json!({"kind":"file_read","task":r.task_id,"index":r.list_index,"offset":r.pos_offset,"length":r.requested_len,"flags":r.flags})
        }
        Some(Req::CancelRequest(r)) => json!({"kind":"file_release","task":r.task_id}),
        Some(Req::AutoSaveComplete(r)) => {
            json!({"kind":"auto_saved","total":r.total_count,"success":r.success_count})
        }
        _ => json!({"kind":"other"}),
    }
}
pub fn response_summary(r: &pb::ClipboardResponse) -> serde_json::Value {
    use serde_json::json;
    match &r.which {
        Some(Rsp::FormatListResponse(_)) => json!({"kind":"formats"}),
        Some(Rsp::FormatDataConfirm(r)) => {
            json!({"kind":"confirm","blocks":r.block_count,"error":r.err})
        }
        Some(Rsp::DataBlockConfirm(r)) => {
            json!({"kind":"block_confirm","block":r.block_id,"error":r.err})
        }
        _ => json!({"kind":"other"}),
    }
}

struct Blocks {
    started: std::time::Instant,
    format: u32,
    count: Option<usize>,
    bytes: usize,
    parts: BTreeMap<i32, Vec<u8>>,
}
impl Default for Blocks {
    fn default() -> Self {
        Self {
            started: std::time::Instant::now(),
            format: 13,
            count: None,
            bytes: 0,
            parts: Default::default(),
        }
    }
}
struct Sending {
    started: std::time::Instant,
    key: String,
    offset: usize,
    block: i32,
}
#[derive(Default)]
pub struct ClipboardData {
    pub image: Option<Vec<u8>>,
    pub error: Option<String>,
    offered: Vec<u8>,
    read_finished: bool,
    sending: Option<Sending>,
    outgoing: Vec<Vec<u8>>,
    formats: Vec<pb::ClipboardFormat>,
    pending: HashMap<String, Blocks>,
}
fn request(value: Req) -> Vec<u8> {
    wire::rpc(pb::rpc_request::Which::ClipRequest(pb::ClipboardRequest {
        which: Some(value),
    }))
    .1
}
fn response(id: i64, value: Rsp) -> Vec<u8> {
    wire::response(
        id,
        pb::rpc_response::Which::ClipResponse(pb::ClipboardResponse { which: Some(value) }),
    )
}
impl ClipboardData {
    pub fn validate_remote_image_read(&self) -> Result<()> {
        ensure!(
            self.offered.is_empty() || self.read_finished,
            "远端尚未取走本机提供的图片；请先在远端粘贴，再读取远端剪贴板"
        );
        Ok(())
    }
    pub fn tick(&mut self) {
        let limit = std::time::Duration::from_secs(10);
        if self
            .sending
            .as_ref()
            .is_some_and(|s| s.started.elapsed() > limit)
        {
            self.sending = None;
            self.error = Some("等待远端图片分块确认超时，请重新粘贴".into());
        }
        if self.pending.values().any(|b| b.started.elapsed() > limit) {
            self.pending.clear();
            self.error = Some("读取远端剪贴板超时".into());
        }
    }

    pub fn clear_offer(&mut self) {
        self.offered.clear();
        self.read_finished = false;
        self.sending = None;
        self.outgoing.clear();
    }
    pub fn ask_image(&mut self) -> Vec<u8> {
        let format = self
            .formats
            .iter()
            .find(|f| f.id == 17)
            .or_else(|| self.formats.iter().find(|f| f.id == 8))
            .map_or(17, |f| f.id);
        let key = uuid::Uuid::new_v4().to_string();
        self.pending.clear();
        self.pending.insert(
            key.clone(),
            Blocks {
                format,
                ..Default::default()
            },
        );
        request(Req::FormatDataAsk(pb::ClipboardFormatDataAsk {
            format_id: format,
            block_key: key,
            format_name: String::new(),
        }))
    }
    pub fn offer_image(&mut self, png: &[u8]) -> Result<(i64, Vec<u8>)> {
        self.offered = super::clipboard_image::to_dib(&super::clipboard_image::from_png(png)?)?;
        self.sending = None;
        self.read_finished = false;
        Ok(wire::rpc(pb::rpc_request::Which::ClipRequest(
            pb::ClipboardRequest {
                which: Some(Req::FormatList(pb::ClipboardFormatListRequest {
                    formats: vec![pb::ClipboardFormat {
                        id: 17,
                        name: String::new(),
                    }],
                    ..Default::default()
                })),
            },
        )))
    }
    pub fn take_outgoing(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outgoing)
    }
    fn next_block(&mut self) {
        let Some(sending) = &mut self.sending else {
            return;
        };
        if sending.offset >= self.offered.len() {
            self.sending = None;
            self.read_finished = true;
            return;
        }
        let end = (sending.offset + 65536).min(self.offered.len());
        sending.block += 1;
        sending.started = std::time::Instant::now();
        self.outgoing
            .push(request(Req::DataBlock(pb::ClipboardDataBlock {
                block_key: sending.key.clone(),
                block_id: sending.block,
                data: self.offered[sending.offset..end].to_vec(),
            })));
        sending.offset = end;
    }

    pub fn ask_text(&mut self) -> Vec<u8> {
        let key = uuid::Uuid::new_v4().to_string();
        self.pending.clear();
        self.pending.insert(key.clone(), Blocks::default());
        request(Req::FormatDataAsk(pb::ClipboardFormatDataAsk {
            format_id: 13,
            block_key: key,
            format_name: String::new(),
        }))
    }
    pub fn request(
        &mut self,
        id: i64,
        r: pb::ClipboardRequest,
        enabled: bool,
    ) -> Result<(Vec<Vec<u8>>, Option<String>)> {
        match r.which {
            Some(Req::FormatList(r)) => {
                self.clear_offer();
                let mut messages = vec![response(
                    id,
                    Rsp::FormatListResponse(pb::ClipboardFormatListResponse {}),
                )];
                self.formats = r.formats.clone();
                self.pending.clear();
                if enabled && r.formats.iter().any(|f| f.id == 13) {
                    let key = uuid::Uuid::new_v4().to_string();
                    self.pending.insert(key.clone(), Blocks::default());
                    messages.push(request(Req::FormatDataAsk(pb::ClipboardFormatDataAsk {
                        format_id: 13,
                        block_key: key,
                        format_name: String::new(),
                    })));
                }
                if enabled
                    && !r.formats.iter().any(|f| f.id == 13)
                    && r.formats.iter().any(|f| [8, 17].contains(&f.id))
                {
                    messages.push(self.ask_image());
                }
                Ok((messages, None))
            }
            Some(Req::DataBlock(r)) => {
                let Some(blocks) = self.pending.get_mut(&r.block_key) else {
                    return Ok((
                        vec![response(
                            id,
                            Rsp::DataBlockConfirm(pb::ClipboardDataBlockConfirm {
                                block_key: r.block_key,
                                block_id: r.block_id,
                                err: 2,
                            }),
                        )],
                        None,
                    ));
                };
                ensure!(r.block_id >= 1 && r.block_id <= 512, "剪贴板分块编号无效");
                if let Some(previous) = blocks.parts.get(&r.block_id) {
                    ensure!(*previous == r.data, "剪贴板重复分块内容不一致");
                }
                if !blocks.parts.contains_key(&r.block_id) {
                    ensure!(
                        blocks.bytes + r.data.len()
                            <= if blocks.format == 13 {
                                2 * 1024 * 1024
                            } else {
                                34 * 1024 * 1024
                            },
                        "剪贴板数据超过限制"
                    );
                    blocks.bytes += r.data.len();
                    blocks.parts.insert(r.block_id, r.data);
                }
                let messages = vec![response(
                    id,
                    Rsp::DataBlockConfirm(pb::ClipboardDataBlockConfirm {
                        block_key: r.block_key.clone(),
                        block_id: r.block_id,
                        err: 1,
                    }),
                )];
                let text = self.finish(&r.block_key)?;
                Ok((messages, text))
            }
            Some(Req::FormatDataAsk(r)) => {
                let supported = r.format_id == 17
                    && !self.offered.is_empty()
                    && r.block_key.len() <= 128
                    && self.sending.is_none();
                crate::perf::trace(
                    "clipboard_offer_read",
                    serde_json::json!({"supported":supported,"format":r.format_id,"offered_bytes":self.offered.len(),"key_bytes":r.block_key.len(),"busy":self.sending.is_some()}),
                );
                let mut messages = vec![response(
                    id,
                    Rsp::FormatDataConfirm(pb::ClipboardFormatDataConfirm {
                        err: if supported { 1 } else { 2 },
                        block_key: r.block_key.clone(),
                        block_count: if supported {
                            self.offered.len().div_ceil(65536) as i32
                        } else {
                            0
                        },
                    }),
                )];
                if supported {
                    self.sending = Some(Sending {
                        started: std::time::Instant::now(),
                        key: r.block_key,
                        offset: 0,
                        block: 0,
                    });
                    self.next_block();
                    messages.extend(self.take_outgoing());
                }
                Ok((messages, None))
            }
            _ => Ok((vec![], None)),
        }
    }
    pub fn response(&mut self, r: pb::ClipboardResponse) -> Result<Option<String>> {
        if let Some(Rsp::DataBlockConfirm(block)) = &r.which
            && self
                .sending
                .as_ref()
                .is_some_and(|s| s.key == block.block_key && s.block == block.block_id)
        {
            if block.err == 1 {
                self.next_block();
            } else {
                self.sending = None;
                self.error = Some("远端拒绝图片分块".into());
            }
        }
        if let Some(Rsp::FormatDataConfirm(r)) = r.which {
            if r.err != 1 {
                if let Some(blocks) = self.pending.remove(&r.block_key) {
                    self.error = Some(
                        if blocks.format == 13 {
                            "远端剪贴板未提供文字"
                        } else {
                            "远端剪贴板未提供所请求的图片格式"
                        }
                        .into(),
                    );
                }
                return Ok(None);
            }
            ensure!((0..=512).contains(&r.block_count), "剪贴板分块数量无效");
            if let Some(blocks) = self.pending.get_mut(&r.block_key) {
                blocks.count = Some(r.block_count as usize);
            }
            return self.finish(&r.block_key);
        }
        Ok(None)
    }
    fn finish(&mut self, key: &str) -> Result<Option<String>> {
        let Some(b) = self.pending.get(key) else {
            return Ok(None);
        };
        let Some(count) = b.count else {
            return Ok(None);
        };
        if b.parts.len() != count {
            return Ok(None);
        }
        ensure!(
            b.parts.keys().enumerate().all(|(i, n)| i as i32 + 1 == *n),
            "剪贴板分块不连续"
        );
        let b = self.pending.remove(key).unwrap();
        let bytes = b.parts.into_values().flatten().collect::<Vec<_>>();
        if b.format != 13 {
            self.image = Some(super::clipboard_image::png(
                &super::clipboard_image::from_dib(&bytes)?,
            )?);
            return Ok(None);
        }
        ensure!(bytes.len() % 2 == 0, "Unicode 剪贴板长度无效");
        let chars = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        Ok(Some(String::from_utf16(&chars)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    #[test]
    fn image_round_trip_uses_acknowledged_bounded_blocks() {
        let image = image::RgbaImage::from_pixel(240, 240, image::Rgba([12, 34, 56, 128]));
        let png = super::super::clipboard_image::png(&image).unwrap();
        let mut peers = [ClipboardData::default(), ClipboardData::default()];
        let (_, offer) = peers[0].offer_image(&png).unwrap();
        let mut queue = std::collections::VecDeque::from([(1usize, offer)]);
        let mut blocks = 0;
        while let Some((target, bytes)) = queue.pop_front() {
            assert!(blocks < 16, "unbounded block loop");
            let message = pb::Message::decode(bytes.as_slice()).unwrap();
            let outgoing = match message.which.unwrap() {
                pb::message::Which::RpcRequest(r) => {
                    let id = r.request_header.unwrap().request_id;
                    let Some(pb::rpc_request::Which::ClipRequest(r)) = r.which else {
                        panic!("wrong RPC")
                    };
                    if let Some(Req::DataBlock(b)) = &r.which {
                        blocks += 1;
                        assert!(b.data.len() <= 65536);
                    }
                    peers[target].request(id, r, true).unwrap().0
                }
                pb::message::Which::RpcResponse(r) => {
                    let Some(pb::rpc_response::Which::ClipResponse(r)) = r.which else {
                        panic!("wrong RPC")
                    };
                    peers[target].response(r).unwrap();
                    peers[target].take_outgoing()
                }
                _ => panic!("wrong transport"),
            };
            queue.extend(outgoing.into_iter().map(|bytes| (1 - target, bytes)));
        }
        assert!(blocks > 1);
        assert!(peers[0].sending.is_none());
        assert_eq!(
            super::super::clipboard_image::from_png(peers[1].image.as_ref().unwrap()).unwrap(),
            image
        );
    }
    fn block(key: &str, id: i32, data: Vec<u8>) -> pb::ClipboardRequest {
        pb::ClipboardRequest {
            which: Some(Req::DataBlock(pb::ClipboardDataBlock {
                block_key: key.into(),
                block_id: id,
                data,
            })),
        }
    }
    #[test]
    fn reordered_utf16_blocks_and_surrogate_pairs_are_reassembled() {
        let mut c = ClipboardData::default();
        c.pending.insert("k".into(), Blocks::default());
        let bytes = "你好 🦀\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert!(
            c.request(1, block("k", 2, bytes[5..].to_vec()), true)
                .unwrap()
                .1
                .is_none()
        );
        assert!(
            c.request(2, block("k", 1, bytes[..5].to_vec()), true)
                .unwrap()
                .1
                .is_none()
        );
        let text = c
            .response(pb::ClipboardResponse {
                which: Some(Rsp::FormatDataConfirm(pb::ClipboardFormatDataConfirm {
                    block_key: "k".into(),
                    block_count: 2,
                    err: 1,
                })),
            })
            .unwrap();
        assert_eq!(text.as_deref(), Some("你好 🦀"));
        assert!(c.pending.is_empty());
    }
    #[test]
    fn unsolicited_blocks_and_out_of_range_indices_are_not_applied() {
        let mut c = ClipboardData::default();
        assert!(
            c.request(1, block("missing", 1, vec![65, 0]), true)
                .unwrap()
                .1
                .is_none()
        );
        c.pending.insert("k".into(), Blocks::default());
        assert!(c.request(2, block("k", 0, vec![]), true).is_err());
        assert!(c.request(2, block("k", 513, vec![]), true).is_err());
    }
}
