//! Capability observations distinguish local implementation from remote evidence.
use anyhow::{Result, ensure};
use cpal::traits::{DeviceTrait, HostTrait};
use serde_json::{Value, json};

/// None means not reported, never an inferred negative capability.
pub fn capture_supported(caps: &Value, request: &Value) -> Option<bool> {
    let rows = caps["video_codecs"].as_array()?;
    if caps["complete"] != true
        || rows.is_empty()
        || rows.iter().any(|v| {
            !v["video_codec"].is_i64() || !v["chroma_sampling"].is_i64() || !v["bit_depth"].is_i64()
        })
    {
        return None;
    }
    Some(rows.iter().any(|row| {
        request["codec"]
            .as_str()
            .is_none_or(|codec| row["video_codec"] == if codec == "h265" { 2 } else { 1 })
            && request["true_color"]
                .as_bool()
                .is_none_or(|enabled| row["chroma_sampling"] == if enabled { 3 } else { 1 })
            && (request["hdr"] != true || row["bit_depth"].as_i64().unwrap_or(0) >= 10)
    }))
}
pub fn validate_capture(caps: &Value, request: &Value) -> Result<()> {
    ensure!(
        capture_supported(caps, request) != Some(false),
        "远端本次会话上报的编码、色度或位深能力不满足该设置；请查看 uu_capabilities_get 的 remote.streamer"
    );
    Ok(())
}

pub fn audio_devices() -> Value {
    let host = cpal::default_host();
    let default_input = host.default_input_device().and_then(|d| d.id().ok());
    let default_output = host.default_output_device().and_then(|d| d.id().ok());
    let pulse = crate::core::pulse_audio::devices().ok();
    let enumerate = |input: bool| {
        if let Some(pulse) = &pulse {
            return json!({"devices":pulse[if input {"input"} else {"output"}]});
        }

        let devices = if input {
            host.input_devices()
        } else {
            host.output_devices()
        };
        match devices {
            Ok(devices) => json!({"devices": devices.take(128).map(|d| {
                let id = d.id().ok();
                let default = if input { &default_input } else { &default_output };
                json!({"id":id.as_ref().map(ToString::to_string),"name":d.description().ok().map(|v|v.name().to_owned()),"default":id.is_some() && &id==default})
            }).collect::<Vec<_>>()}),
            Err(e) => json!({"devices":[],"error":e.to_string()}),
        }
    };
    json!({"input":enumerate(true),"output":enumerate(false)})
}

pub fn local() -> Value {
    let initialized = ffmpeg_next::init().is_ok();
    let decoders = [("h264", ffmpeg_next::codec::Id::H264), ("h265", ffmpeg_next::codec::Id::HEVC)]
        .into_iter().map(|(name,id)| json!({"codec":name,"software_available":initialized && ffmpeg_next::decoder::find(id).is_some()})).collect::<Vec<_>>();
    json!({"platform":"linux","decoders":decoders,"decoder_profiles":crate::core::decoder::profiles(),"hardware_decode":crate::core::decoder::capabilities(),"audio_devices":audio_devices(),
        "max_sessions":8,"display":{"scope":"session","screen_ids":"remote_stable_ids","hdr_presentation":false,"hdr_to_sdr":crate::core::pixels::RgbaConverter::hdr_supported(),"presentation":"rgba8_sdr"},
        "implemented":{"touch":true,"playback":true,"microphone":true,"clipboard_formats":["text","image","files"],"desktop_drop":{"local_to_remote":"ole_drop","remote_to_local":"pending_native_drag_source","wayland_destination":"verified_uri_drop"},"encoded_streams":["h264","h265","opus"],"hardware_decode":true},
        "evidence_policy":"unknown remote capability is not unsupported"})
}

pub fn session_status(s: &crate::service::Managed, args: &Value) -> Result<Value> {
    const SECTIONS: &[&str] = &[
        "performance",
        "decoder",
        "display",
        "audio",
        "microphone",
        "streams",
        "leases",
    ];
    let selected: Vec<&str> = match args.get("include") {
        None => SECTIONS.to_vec(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| {
                let name = v.as_str().unwrap_or("");
                ensure!(SECTIONS.contains(&name), "未知状态分区：{name}");
                Ok(name)
            })
            .collect::<Result<_>>()?,
        _ => anyhow::bail!("include 必须为分区名称数组"),
    };
    let mut value = s.status();
    for section in selected {
        value[section] = match section {
            "performance" => s.snapshot.get("stats").cloned().unwrap_or(Value::Null),
            "display" => {
                json!({"scope":"session","screens":s.snapshot.get("screens"),"requested":s.snapshot.get("display_requested"),"confirmed":s.snapshot.get("remote_capture"),"actual":s.frame.as_ref().map(|f|json!({"geometry":f.geometry,"width":f.width,"height":f.height,"sequence":f.sequence,"age_ms":f.decoded_at.elapsed().as_millis() as u64}))})
            }
            "decoder" => s.backend.media.decoder.lock().unwrap().clone(),
            "audio" => s
                .snapshot
                .get("audio_status")
                .cloned()
                .unwrap_or(json!({"state":"not_observed"})),
            "microphone" => s
                .snapshot
                .get("microphone_status")
                .cloned()
                .unwrap_or(json!({"state":"inactive"})),
            "streams" => {
                json!({"subscriptions":s.resource_leases.values().filter(|(_,kind)|kind=="stream").count()})
            }
            "leases" => {
                json!({"clients":s.owners.len(),"media":s.video_owners.len(),"resources":s.resource_leases.values().map(|(_,kind)|kind).collect::<Vec<_>>()})
            }
            _ => unreachable!(),
        };
    }
    if let Some(audio) = value.get_mut("audio") {
        audio["negotiation"] = s
            .snapshot
            .get("audio_negotiation")
            .cloned()
            .unwrap_or(Value::Null);
    }
    Ok(value)
}

pub fn get(hub: &crate::service::Shared, client: &str, args: &Value) -> Result<Value> {
    let mut value = json!({"local":local()});
    if let Some(id) = args.get("session_id") {
        let id = id
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("session_id 无效"))?;
        let h = hub.lock().unwrap();
        let s = h.owner(client, id)?;
        value["session_id"] = json!(id);
        value["remote"] = json!({"encoder":s.snapshot.get("remote_encoders"),"features":s.snapshot.get("remote_features"),"general":s.snapshot.get("remote_general"),"streamer":s.snapshot.get("remote_streamer"),"screens":s.snapshot.get("screens")});
        let decoded = s.backend.media.decoder.lock().unwrap().clone();
        let encoders = s
            .snapshot
            .get("remote_encoders")
            .and_then(|v| v["encoders"].as_array());
        let codecs=[("h264",1),("h265",2)].into_iter().map(|(name,code)|{
            let local=value["local"]["decoders"].as_array().unwrap().iter().find(|v|v["codec"]==name).is_some_and(|v|v["software_available"]==true);
            let reported = s.snapshot.get("remote_streamer").and_then(|caps|capture_supported(caps,&json!({"codec":name})));
            let remote=if decoded["codec"]==name {Some(true)} else {reported.or_else(||encoders.map(|v|v.iter().any(|e|e["codec_type"]==code)))};
            json!({"codec":name,"local_supported":local,"remote_supported":remote,"available":if local {remote} else {Some(false)},"evidence":if decoded["codec"]==name {"received_and_decoded"} else if reported.is_some() {"current_session_streamer_capabilities"} else if encoders.is_some() {"remote_encoder_capabilities"} else {"not_reported"}})
        }).collect::<Vec<_>>();
        let microphone_transport = s
            .snapshot
            .get("audio_negotiation")
            .and_then(|v| v["tracks"].as_array())
            .map(|tracks| {
                tracks
                    .iter()
                    .any(|t| t["negotiated"] == "sendrecv" || t["negotiated"] == "sendonly")
            });
        value["joint"] = json!({"touch":s.snapshot.get("remote_general").and_then(|v|v["support_touch"].as_bool()),"video_codecs":codecs,"microphone":{"local_implemented":true,"transport_negotiated":microphone_transport,"virtual_driver_policy":s.snapshot.get("microphone_policy"),"remote_audio_observed":false,"remote_supported":Value::Null}});
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_requires_a_reported_combination_and_unknown_is_not_unsupported() {
        let caps = json!({"complete":true,"video_codecs":[{"video_codec":1,"chroma_sampling":1,"bit_depth":8},{"video_codec":2,"chroma_sampling":3,"bit_depth":10}]});
        assert_eq!(
            capture_supported(&Value::Null, &json!({"codec":"h265"})),
            None
        );
        assert_eq!(
            capture_supported(&caps, &json!({"codec":"h264","hdr":true})),
            Some(false)
        );
        assert_eq!(
            capture_supported(&caps, &json!({"codec":"h265","true_color":true,"hdr":true})),
            Some(true)
        );
        assert_eq!(
            capture_supported(
                &json!({"complete":false,"video_codecs":caps["video_codecs"]}),
                &json!({"codec":"h265"})
            ),
            None
        );
    }
}
