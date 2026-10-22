use super::{cloud::now_ms, pb};
use prost::Message;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, Ordering};

static SEQUENCE: AtomicI64 = AtomicI64::new(1);

/// webrtc-rs omits max-message-size, which means 64 KiB to libwebrtc. UU sends
/// 200 KiB FTP blocks and otherwise silently refuses to enqueue a whole block.
pub fn data_channel_limit(sdp: &str) -> String {
    let mut result = String::new();
    let mut application = false;
    for line in sdp.lines() {
        if line.starts_with("m=") {
            if application {
                result.push_str("a=max-message-size:2097152\r\n");
            }
            application = line.starts_with("m=application ");
        }
        if application && line.starts_with("a=max-message-size:") {
            continue;
        }
        result.push_str(line);
        result.push_str("\r\n");
    }
    if application {
        result.push_str("a=max-message-size:2097152\r\n");
    }
    result
}

pub fn features() -> pb::FeatureFlag {
    pb::FeatureFlag {
        ff_capture_setting: 4,
        ff_simple_action: 3,
        ff_system_metrics: 3,
        ff_file_transfer_ftp: 2,
        ff_file_transfer_ftp2: 2,
        ff_clipboard: 3,
        ff_qos_stat: 1,
        ..Default::default()
    }
}

pub fn wrap(value: pb::message::Which) -> Vec<u8> {
    pb::Message {
        seq: SEQUENCE.fetch_add(1, Ordering::Relaxed),
        timestamp: now_ms(),
        which: Some(value),
    }
    .encode_to_vec()
}

pub fn action(number: i32) -> Vec<u8> {
    let value = pb::message::Which::SimpleAction(pb::SimpleAction {
        action: number,
        params: if number <= 1 {
            Some(pb::simple_action::Params::FeatureFlag(features()))
        } else {
            None
        },
        ..Default::default()
    });
    if number == 0 {
        pb::Message {
            seq: 0,
            timestamp: 0,
            which: Some(value),
        }
        .encode_to_vec()
    } else {
        wrap(value)
    }
}

pub fn rpc(value: pb::rpc_request::Which) -> (i64, Vec<u8>) {
    let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    (
        id,
        wrap(pb::message::Which::RpcRequest(pb::RpcRequest {
            request_header: Some(pb::RequestHeader { request_id: id }),
            which: Some(value),
        })),
    )
}

/// A metadata-only lookup of our random, nonexistent test directory. Unlike
/// handshake ECHO, FTP RPC responses carry the original request ID.
pub fn lookup_probe() -> (i64, Vec<u8>) {
    rpc(pb::rpc_request::Which::FileTransferFtpRequest(
        pb::FileTransferFtpRequest {
            which: Some(pb::file_transfer_ftp_request::Which::ReadDir(pb::ReadDir {
                id: Some(pb::TaskId {
                    task_id: 0,
                    file_index: 0,
                }),
                path: format!("C:\\uu-perf-probe-{}", uuid::Uuid::new_v4()),
            })),
        },
    ))
}

/// Use the host's selected screen and advertised track, never a guessed ID.
pub fn selected_video_track(sources: &pb::ScreenSources) -> Option<i32> {
    sources
        .screens
        .iter()
        .find(|screen| screen.id == sources.current_screen_id)
        .filter(|screen| screen.video_track_index >= 0)
        .map(|screen| screen.video_track_index)
}

pub fn response(id: i64, value: pb::rpc_response::Which) -> Vec<u8> {
    wrap(pb::message::Which::RpcResponse(pb::RpcResponse {
        response_header: Some(pb::ResponseHeader { request_id: id }),
        which: Some(value),
    }))
}

pub fn options(device: &str, settings: &Value) -> Vec<u8> {
    let fps = settings["fps"].as_i64().unwrap_or(60) as i32;
    // Explicit startup experiment: establish the supported file session before
    // asking for desktop capture over the authenticated application channel.
    let capture_type = if settings["startup_file_only"] == true {
        pb::connect_options::Type::CtFiletransfer as i32
    } else {
        pb::connect_options::Type::CtDesktop as i32
    };
    // The official constructor fills local_resolution even when following the
    // remote screen. Keep this explicit comparison separate from resolution
    // selection until live tests establish a startup benefit.
    let local_resolution = settings["startup_local_resolution"]
        .as_array()
        .filter(|size| size.len() == 2)
        .and_then(|size| Some((size[0].as_i64()?, size[1].as_i64()?)))
        .filter(|(w, h)| (320..=7680).contains(w) && (200..=4320).contains(h))
        .map(|(width, height)| pb::ScreenResolution {
            width: width as i32,
            height: height as i32,
        });
    crate::perf::trace(
        "connect_capture_options",
        json!({"capture_type":capture_type,"resolution_type":3,"local_width":local_resolution.as_ref().map(|s|s.width),
            "local_height":local_resolution.as_ref().map(|s|s.height)}),
    );
    pb::ConnectOptions {
        capture_type,
        type_value: -1,
        capture_params: Some(pb::CaptureParams {
            fps: fps_enum(fps),
            fps_count: fps,
            video_quality: settings["quality"].as_i64().unwrap_or(2) as i32,
            cursor_capture: true,
            choose_resolution_type: 3,
            local_resolution,
            chroma_format: 1,
            max_custom_bitrate: settings["bitrate"].as_i64().unwrap_or(0) as i32,
            ..Default::default()
        }),
        decoder_cap_list: [1, 2]
            .into_iter()
            .flat_map(|codec_type| {
                [1, 3].into_iter().map(move |chroma_format| pb::DecoderCap {
                    fps,
                    codec_type,
                    resolution_width: 3840,
                    resolution_height: 2160,
                    chroma_format,
                })
            })
            .collect(),
        client_type: 3,
        device_id: device.into(),
        control_connect_type: if settings["assistance"].as_bool().unwrap_or(false) {
            2
        } else {
            1
        },
        feature_flag: Some(features()),
        client_version: "4.33.0.8907".into(),
        ..Default::default()
    }
    .encode_to_vec()
}

pub fn fps_enum(fps: i32) -> i32 {
    match fps {
        30 => 1,
        60 => 2,
        90 => 3,
        144 => 4,
        _ => 0,
    }
}

pub fn streamer(control: &str, fps: i64) -> String {
    json!({"control_id":control,"device_capability":{
        "display_info":[{"id":0,"fps":fps,"type":0,"hdr":0}],
        "video_codec_capability":super::decoder::profiles(),
        "ice_id":""
    }})
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_resolution_diagnostic_keeps_remote_resolution_selection() {
        for (value, expected) in [
            (json!(null), None),
            (json!([3840, 2160]), Some((3840, 2160))),
            (json!([0, 0]), None),
            (json!([1920, 1080, 60]), None),
            (json!([1920, "1080"]), None),
            (json!([i64::MAX, 1080]), None),
        ] {
            let wire = options("local", &json!({"startup_local_resolution":value}));
            let options = pb::ConnectOptions::decode(wire.as_slice()).unwrap();
            let capture = options.capture_params.unwrap();
            assert_eq!(capture.choose_resolution_type, 3);
            assert!(capture.choose_resolution.is_none());
            assert!(!options.force_virtual_display);
            assert_eq!(
                capture.local_resolution.map(|s| (s.width, s.height)),
                expected
            );
        }
    }
    #[test]
    fn diagnostic_lookup_is_read_only_and_has_a_unique_rpc_id() {
        let (id, bytes) = lookup_probe();
        let message = pb::Message::decode(bytes.as_slice()).unwrap();
        let Some(pb::message::Which::RpcRequest(request)) = message.which else {
            panic!()
        };
        assert_eq!(request.request_header.unwrap().request_id, id);
        let Some(pb::rpc_request::Which::FileTransferFtpRequest(ftp)) = request.which else {
            panic!()
        };
        let Some(pb::file_transfer_ftp_request::Which::ReadDir(directory)) = ftp.which else {
            panic!()
        };
        let suffix = directory.path.strip_prefix("C:\\uu-perf-probe-").unwrap();
        assert!(uuid::Uuid::parse_str(suffix).is_ok());
        assert_ne!(lookup_probe().0, id);
    }
    #[test]
    fn video_start_uses_only_the_advertised_selected_track() {
        let mut screens = pb::ScreenSources {
            current_screen_id: 42,
            screens: vec![
                pb::Screen {
                    id: 7,
                    video_track_index: 5,
                    ..Default::default()
                },
                pb::Screen {
                    id: 42,
                    video_track_index: 0,
                    ..Default::default()
                },
            ],
        };
        assert_eq!(selected_video_track(&screens), Some(0));
        screens.screens[1].video_track_index = -1;
        assert_eq!(selected_video_track(&screens), None);
        screens.current_screen_id = 99;
        assert_eq!(selected_video_track(&screens), None);
        assert_eq!(selected_video_track(&pb::ScreenSources::default()), None);
    }
    #[test]
    fn assistance_is_explicitly_identified_to_the_host() {
        let normal = pb::ConnectOptions::decode(options("local", &json!({})).as_slice()).unwrap();
        let assist =
            pb::ConnectOptions::decode(options("local", &json!({"assistance":true})).as_slice())
                .unwrap();
        assert_eq!(
            normal.control_connect_type,
            pb::ControlConnectType::Normal as i32
        );
        assert_eq!(
            assist.control_connect_type,
            pb::ControlConnectType::Assistance as i32
        );
        assert_eq!(assist.device_id, "local");
    }
    #[test]
    fn file_startup_diagnostic_preserves_media_and_authorization_options() {
        let normal = pb::ConnectOptions::decode(options("local", &json!({})).as_slice()).unwrap();
        let mut file = pb::ConnectOptions::decode(
            options("local", &json!({"startup_file_only":true})).as_slice(),
        )
        .unwrap();
        assert_eq!(
            file.capture_type,
            pb::connect_options::Type::CtFiletransfer as i32
        );
        file.capture_type = normal.capture_type;
        assert_eq!(file, normal);
    }
    #[test]
    fn large_message_limit_only_changes_application_section() {
        let source = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=sendrecv\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=sctp-port:5000\r\na=max-message-size:65536\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=recvonly\r\n";
        let result = data_channel_limit(source);
        assert_eq!(result.matches("max-message-size").count(), 1);
        assert!(result.contains("a=sctp-port:5000\r\na=max-message-size:2097152\r\nm=audio"));
        assert_eq!(data_channel_limit(&result), result);
        assert!(
            !data_channel_limit("v=0\r\nm=video 9 RTP/AVP 102\r\n").contains("max-message-size")
        );
    }
    #[test]
    fn probe_and_rpc_envelopes_match_the_wire_contract() {
        let probe = pb::Message::decode(action(0).as_slice()).unwrap();
        assert_eq!((probe.seq, probe.timestamp), (0, 0));
        assert!(matches!(
            probe.which,
            Some(pb::message::Which::SimpleAction(pb::SimpleAction {
                action: 0,
                ..
            }))
        ));
        let answer = pb::Message::decode(
            response(
                123,
                pb::rpc_response::Which::TextChangeResponse(pb::ClipboardTextChangeResponse {
                    err: 1,
                }),
            )
            .as_slice(),
        )
        .unwrap();
        let Some(pb::message::Which::RpcResponse(r)) = answer.which else {
            panic!()
        };
        assert_eq!(r.response_header.unwrap().request_id, 123);
    }
}
