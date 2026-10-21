//! Local interoperability probe. SDP is exchanged over stdin/stdout; no account or UU server.
use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{io::Write, sync::Arc, time::Instant};
use tokio::{
    sync::mpsc,
    time::{Duration, timeout},
};
use webrtc::{
    api::{
        APIBuilder,
        setting_engine::{SctpMaxMessageSize, SettingEngine},
    },
    data_channel::data_channel_init::RTCDataChannelInit,
    peer_connection::{
        configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription,
    },
};

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let separate = arguments.iter().any(|arg| arg == "--separate-input");
    let shared = arguments.iter().any(|arg| arg == "--shared-input");
    let idle_large = arguments.iter().any(|arg| arg == "--idle-large");
    ensure!(
        usize::from(separate) + usize::from(shared) + usize::from(idle_large) <= 1,
        "choose one transport comparison"
    );
    let mut settings = SettingEngine::default();
    settings.detach_data_channels();
    settings.set_sctp_max_message_size_can_send(SctpMaxMessageSize::Bounded(2 * 1024 * 1024));
    let api = APIBuilder::new().with_setting_engine(settings).build();
    let pc = api.new_peer_connection(RTCConfiguration::default()).await?;
    let result = timeout(Duration::from_secs(60), async {
        let (opened, mut ready) = mpsc::channel(2);
        let control = pc.create_data_channel("CONTROL_DATA_CHANNEL", None).await?;
        let control_opened = opened.clone();
        control.on_open(Box::new(move || {
            let opened = control_opened.clone();
            Box::pin(async move {
                let _ = opened.send(()).await;
            })
        }));
        let mut text_channel = None;
        for label in [
            "TEXT_DATA_CHANNEL",
            "STREAMER_DATA_CHANNEL",
            "FILE_DATA_CHANNEL",
            "BINARY_DATA_CHANNEL",
        ] {
            let channel = pc
                .create_data_channel(
                    label,
                    Some(RTCDataChannelInit {
                        ordered: Some(label != "BINARY_DATA_CHANNEL"),
                        ..Default::default()
                    }),
                )
                .await?;
            if label == "TEXT_DATA_CHANNEL" {
                if separate {
                    let text_opened = opened.clone();
                    channel.on_open(Box::new(move || {
                        let opened = text_opened.clone();
                        Box::pin(async move {
                            let _ = opened.send(()).await;
                        })
                    }));
                }
                text_channel = Some(channel);
            }
        }
        let mut gathered = pc.gathering_complete_promise().await;
        pc.set_local_description(pc.create_offer(None).await?)
            .await?;
        gathered.recv().await;
        println!(
            "{}",
            serde_json::to_string(&pc.local_description().await.context("missing offer")?)?
        );
        std::io::stdout().flush()?;
        let answer = tokio::task::spawn_blocking(|| -> Result<RTCSessionDescription> {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(serde_json::from_str(&line)?)
        })
        .await??;
        pc.set_remote_description(answer).await?;
        ready.recv().await.context("channel did not open")?;
        if separate {
            ready.recv().await.context("second channel did not open")?;
        }
        let channel = control.detach().await?;
        if idle_large {
            let report = idle_large_probe(channel).await?;
            println!("{report}");
            ensure!(
                report["verified"] == true,
                "idle large-message write timed out"
            );
            return Ok(());
        }
        if separate || shared {
            let bulk = if separate {
                text_channel
                    .context("missing text channel")?
                    .detach()
                    .await?
            } else {
                channel.clone()
            };
            let report = concurrent_probe(channel, bulk, separate).await?;
            println!("{report}");
            return Ok(());
        }
        let mut response = vec![0; 256];
        let mut ping_times = vec![];
        let mut bulk_times = vec![];
        let start = Instant::now();
        for sequence in 0_u64..160 {
            let bulk = sequence >= 32;
            let size = if bulk { 64 * 1024 } else { 32 };
            let mut payload: Vec<u8> = (0..size).map(|i| (i as u64 ^ sequence) as u8).collect();
            payload[..8].copy_from_slice(&sequence.to_le_bytes());
            let expected = Sha256::digest(&payload);
            let sent = Instant::now();
            channel
                .write_data_channel(&Bytes::from(payload), false)
                .await?;
            let (n, _) = timeout(
                Duration::from_secs(5),
                channel.read_data_channel(&mut response),
            )
            .await??;
            ensure!(
                &response[..n] == expected.as_slice(),
                "payload mismatch at sequence {sequence}"
            );
            let elapsed = sent.elapsed().as_secs_f64() * 1000.0;
            if bulk {
                bulk_times.push(elapsed);
            } else {
                ping_times.push(elapsed);
            }
        }
        let report = |values: &mut Vec<f64>| {
            values.sort_by(f64::total_cmp);
            json!({"count":values.len(),"median_ms":values[values.len()/2],"max_ms":values.last()})
        };
        println!(
            "{}",
            json!({"event":"transport_result","verified":true,
            "ping":report(&mut ping_times), "bulk_ack":report(&mut bulk_times),
            "bulk_bytes":8*1024*1024,"elapsed_s":start.elapsed().as_secs_f64()})
        );
        Ok::<_, anyhow::Error>(())
    })
    .await;
    pc.close().await?;
    result?
}

type RawChannel = Arc<webrtc::data::data_channel::DataChannel>;

async fn probe_reply(channel: &RawChannel) -> Result<(u64, Vec<u8>, Instant)> {
    let mut response = [0; 256];
    let (n, _) = timeout(
        Duration::from_secs(5),
        channel.read_data_channel(&mut response),
    )
    .await??;
    let received = Instant::now();
    ensure!(n == 40, "invalid acknowledgement length");
    Ok((
        u64::from_le_bytes(response[..8].try_into()?),
        response[8..n].to_vec(),
        received,
    ))
}

fn synthetic_message(sequence: u64, size: usize) -> Vec<u8> {
    let mut payload: Vec<u8> = (0..size).map(|i| (i as u64 ^ sequence) as u8).collect();
    payload[..8].copy_from_slice(&sequence.to_le_bytes());
    payload
}

/// Let the output loop go idle, then exceed the production pending-queue size.
/// The local receiver explicitly negotiates larger messages for this mode.
async fn idle_large_probe(channel: RawChannel) -> Result<serde_json::Value> {
    let mut rows = vec![];
    let mut response = [0; 256];
    for (id, size) in [32, 200 * 1024 + 3, 400 * 1024 + 1, 32]
        .into_iter()
        .enumerate()
    {
        tokio::time::sleep(Duration::from_millis(750)).await;
        let payload = synthetic_message(id as u64, size);
        let expected = Sha256::digest(&payload);
        let started = Instant::now();
        let write = timeout(
            Duration::from_secs(5),
            channel.write_data_channel(&Bytes::from(payload), false),
        )
        .await;
        if write.is_err() {
            rows.push(json!({"bytes":size,"write_timed_out":true,
                "write_ms":started.elapsed().as_secs_f64()*1000.0}));
            return Ok(json!({"event":"transport_result","mode":"idle_large",
                "verified":false,"rounds":rows}));
        }
        ensure!(write?? == size, "incomplete write");
        let write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (n, _) = timeout(
            Duration::from_secs(5),
            channel.read_data_channel(&mut response),
        )
        .await??;
        ensure!(
            &response[..n] == expected.as_slice(),
            "idle-message hash mismatch"
        );
        rows.push(
            json!({"bytes":size,"write_timed_out":false,"write_ms":write_ms,
            "ack_ms":started.elapsed().as_secs_f64()*1000.0}),
        );
    }
    Ok(json!({"event":"transport_result","mode":"idle_large","verified":true,"rounds":rows}))
}

/// A file-sized message followed immediately by a small input-sized message.
/// Both are reliable/ordered and fully hash-checked. Only their stream differs.
async fn concurrent_probe(
    control: RawChannel,
    bulk: RawChannel,
    separate: bool,
) -> Result<serde_json::Value> {
    for id in 0..8 {
        let bytes = synthetic_message(id, 32);
        let expected = Sha256::digest(&bytes);
        control
            .write_data_channel(&Bytes::from(bytes), false)
            .await?;
        let reply = probe_reply(&control).await?;
        ensure!(
            reply.0 == id && reply.1 == expected.as_slice(),
            "warm-up hash mismatch"
        );
    }
    let started = Instant::now();
    let mut rows = vec![];
    for round in 0..5_u64 {
        let file_id = 1000 + round;
        let input_id = 2000 + round;
        let file = synthetic_message(file_id, 64 * 1024);
        let input = synthetic_message(input_id, 32);
        let expected_file = Sha256::digest(&file);
        let expected_input = Sha256::digest(&input);
        let file_started = Instant::now();
        bulk.write_data_channel(&Bytes::from(file), false).await?;
        let input_started = Instant::now();
        control
            .write_data_channel(&Bytes::from(input), false)
            .await?;
        let replies = if separate {
            let (file, input) = tokio::try_join!(probe_reply(&bulk), probe_reply(&control))?;
            [file, input]
        } else {
            [probe_reply(&control).await?, probe_reply(&control).await?]
        };
        let mut input_ms = None;
        let mut file_ms = None;
        for (id, hash, received) in replies {
            if id == input_id && input_ms.is_none() {
                ensure!(hash == expected_input.as_slice(), "input content mismatch");
                input_ms = Some(received.duration_since(input_started).as_secs_f64() * 1000.0);
            } else if id == file_id && file_ms.is_none() {
                ensure!(hash == expected_file.as_slice(), "file content mismatch");
                file_ms = Some(received.duration_since(file_started).as_secs_f64() * 1000.0);
            } else {
                anyhow::bail!("unexpected or duplicate acknowledgement");
            }
        }
        ensure!(
            input_ms.is_some() && file_ms.is_some(),
            "missing acknowledgement"
        );
        rows.push(json!({"round":round,"input_ms":input_ms,"file_ms":file_ms}));
    }
    Ok(
        json!({"event":"transport_result","mode":if separate {"separate"} else {"shared"},
        "verified":true,"rounds":rows,"bulk_bytes":5*64*1024,"elapsed_s":started.elapsed().as_secs_f64()}),
    )
}
