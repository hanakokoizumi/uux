//! Windows CF_UNICODETEXT is UTF-16LE on the block transport. The newer
//! TextChange RPC uses a protobuf UTF-8 string instead.
use super::{pb, wire};
use anyhow::{Result, ensure};
use pb::{clipboard_request::Which as Req, clipboard_response::Which as Rsp};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct Blocks {
    count: Option<usize>,
    bytes: usize,
    parts: BTreeMap<i32, Vec<u8>>,
}
#[derive(Default)]
pub struct TextClipboard {
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
impl TextClipboard {
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
                let mut messages = vec![response(
                    id,
                    Rsp::FormatListResponse(pb::ClipboardFormatListResponse {}),
                )];
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
                ensure!(r.block_id >= 1 && r.block_id <= 256, "剪贴板分块编号无效");
                if !blocks.parts.contains_key(&r.block_id) {
                    ensure!(
                        blocks.bytes + r.data.len() <= 2 * 1024 * 1024,
                        "剪贴板文字超过限制"
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
            Some(Req::FormatDataAsk(r)) => Ok((
                vec![response(
                    id,
                    Rsp::FormatDataConfirm(pb::ClipboardFormatDataConfirm {
                        err: 2,
                        block_key: r.block_key,
                        block_count: 0,
                    }),
                )],
                None,
            )),
            _ => Ok((vec![], None)),
        }
    }
    pub fn response(&mut self, r: pb::ClipboardResponse) -> Result<Option<String>> {
        if let Some(Rsp::FormatDataConfirm(r)) = r.which {
            if r.err != 1 {
                self.pending.remove(&r.block_key);
                return Ok(None);
            }
            ensure!((0..=256).contains(&r.block_count), "剪贴板分块数量无效");
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
        let mut c = TextClipboard::default();
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
        let mut c = TextClipboard::default();
        assert!(
            c.request(1, block("missing", 1, vec![65, 0]), true)
                .unwrap()
                .1
                .is_none()
        );
        c.pending.insert("k".into(), Blocks::default());
        assert!(c.request(2, block("k", 0, vec![]), true).is_err());
        assert!(c.request(2, block("k", 257, vec![]), true).is_err());
    }
}
