//! webrtc-rs 0.17 consumes RTX only for TWCC. Tap authenticated RTP before that
//! drain, restore its original sequence/SSRC, and merge it with primary video.
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use webrtc::{
    interceptor::{stream_info::StreamInfo, *},
    rtp::packet::Packet,
};
pub struct Repair {
    pub packets: mpsc::Sender<Packet>,
    pub recovered: Arc<AtomicU64>,
    pub primary_ssrc: Arc<AtomicU32>,
}
impl InterceptorBuilder for Repair {
    fn build(&self, _: &str) -> Result<Arc<dyn Interceptor + Send + Sync>, Error> {
        Ok(Arc::new(Self {
            packets: self.packets.clone(),
            recovered: self.recovered.clone(),
            primary_ssrc: self.primary_ssrc.clone(),
        }))
    }
}
struct Reader {
    inner: Arc<dyn RTPReader + Send + Sync>,
    tx: mpsc::Sender<Packet>,
    mapping: Option<(u32, u8)>,
    recovered: Arc<AtomicU64>,
}
#[async_trait]
impl RTPReader for Reader {
    async fn read(
        &self,
        buf: &mut [u8],
        attrs: &Attributes,
    ) -> Result<(Packet, Attributes), Error> {
        let (packet, attrs) = self.inner.read(buf, attrs).await?;
        let mut copy = packet.clone();
        if let Some((ssrc, pt)) = self.mapping {
            if copy.payload.len() <= 2 {
                return Ok((packet, attrs));
            }
            copy.header.sequence_number = u16::from_be_bytes([copy.payload[0], copy.payload[1]]);
            copy.header.ssrc = ssrc;
            copy.header.payload_type = pt;
            copy.payload = copy.payload.slice(2..);
            self.recovered.fetch_add(1, Ordering::Relaxed);
        }
        // This stream is already protected by DTLS/SRTP. Backpressure is bounded.
        let _ = self.tx.send(copy).await;
        Ok((packet, attrs))
    }
}
#[async_trait]
impl Interceptor for Repair {
    async fn bind_rtcp_reader(
        &self,
        r: Arc<dyn RTCPReader + Send + Sync>,
    ) -> Arc<dyn RTCPReader + Send + Sync> {
        r
    }
    async fn bind_rtcp_writer(
        &self,
        w: Arc<dyn RTCPWriter + Send + Sync>,
    ) -> Arc<dyn RTCPWriter + Send + Sync> {
        w
    }
    async fn bind_local_stream(
        &self,
        _: &StreamInfo,
        w: Arc<dyn RTPWriter + Send + Sync>,
    ) -> Arc<dyn RTPWriter + Send + Sync> {
        w
    }
    async fn unbind_local_stream(&self, _: &StreamInfo) {}
    async fn unbind_remote_stream(&self, info: &StreamInfo) {
        if info.associated_stream.is_none() && info.mime_type.eq_ignore_ascii_case("video/h264") {
            let _ = self.primary_ssrc.compare_exchange(
                info.ssrc,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }
    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }
    async fn bind_remote_stream(
        &self,
        info: &StreamInfo,
        r: Arc<dyn RTPReader + Send + Sync>,
    ) -> Arc<dyn RTPReader + Send + Sync> {
        if !info.mime_type.starts_with("video/") {
            return r;
        }
        // Negotiation already identifies the primary SSRC before any RTP is
        // readable. Waiting for on_track/first RTP to discover it prevents PLI
        // recovery when the initial keyframe itself never reaches the reader.
        if info.associated_stream.is_none()
            && info.mime_type.eq_ignore_ascii_case("video/h264")
            && info.ssrc != 0
        {
            self.primary_ssrc.store(info.ssrc, Ordering::Relaxed);
            crate::perf::trace(
                "h264_negotiated",
                crate::perf::h264_format(&info.sdp_fmtp_line),
            );
        }
        crate::perf::trace(
            "media_start",
            serde_json::json!({
                "stage":"video_stream_bound","repair":info.associated_stream.is_some()
            }),
        );
        Arc::new(Reader {
            inner: r,
            tx: self.packets.clone(),
            mapping: info
                .associated_stream
                .as_ref()
                .map(|s| (s.ssrc, s.payload_type)),
            recovered: self.recovered.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::interceptor::stream_info::AssociatedStreamInfo;

    struct Unread;
    #[async_trait]
    impl RTPReader for Unread {
        async fn read(&self, _: &mut [u8], _: &Attributes) -> Result<(Packet, Attributes), Error> {
            panic!("keyframe target must be available before the first packet is read");
        }
    }

    #[tokio::test]
    async fn negotiated_primary_is_available_before_rtp_and_not_replaced_by_rtx() {
        let (packets, _) = mpsc::channel(1);
        let repair = Repair {
            packets,
            recovered: Default::default(),
            primary_ssrc: Default::default(),
        };
        let primary = StreamInfo {
            ssrc: 42,
            mime_type: "video/H264".into(),
            ..Default::default()
        };
        let _ = repair.bind_remote_stream(&primary, Arc::new(Unread)).await;
        assert_eq!(repair.primary_ssrc.load(Ordering::Relaxed), 42);
        let rtx = StreamInfo {
            ssrc: 43,
            mime_type: "video/rtx".into(),
            associated_stream: Some(AssociatedStreamInfo {
                ssrc: 42,
                payload_type: 102,
            }),
            ..Default::default()
        };
        let _ = repair.bind_remote_stream(&rtx, Arc::new(Unread)).await;
        assert_eq!(repair.primary_ssrc.load(Ordering::Relaxed), 42);
        repair.unbind_remote_stream(&rtx).await;
        assert_eq!(repair.primary_ssrc.load(Ordering::Relaxed), 42);
        let next = StreamInfo {
            ssrc: 44,
            ..primary.clone()
        };
        let _ = repair.bind_remote_stream(&next, Arc::new(Unread)).await;
        repair.unbind_remote_stream(&primary).await;
        assert_eq!(repair.primary_ssrc.load(Ordering::Relaxed), 44);
        repair.unbind_remote_stream(&next).await;
        assert_eq!(repair.primary_ssrc.load(Ordering::Relaxed), 0);
        let invalid = StreamInfo { ssrc: 0, ..primary };
        let _ = repair.bind_remote_stream(&invalid, Arc::new(Unread)).await;
        assert_eq!(repair.primary_ssrc.load(Ordering::Relaxed), 0);
    }
}
