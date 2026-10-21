//! Local-only burst tolerance comparison. No UU connection or system tuning.
#[path = "../src/core/network.rs"]
mod network;

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::UdpSocket;

const COUNT: usize = 512;
const PACKET_BYTES: usize = 1200;

fn packet(sequence: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..PACKET_BYTES)
        .map(|i| ((sequence * 31 + i) % 251) as u8)
        .collect();
    bytes[..4].copy_from_slice(&(sequence as u32).to_be_bytes());
    bytes
}

async fn burst(requested: usize) -> Result<Value> {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let hook_reports = reports.clone();
    let net = network::system_network(requested, move |report| {
        hook_reports.lock().unwrap().push(report)
    });
    ensure!(!net.is_virtual());
    let receiver = net.bind("127.0.0.1:0".parse()?).await?;
    let sender = UdpSocket::bind("127.0.0.1:0").await?;
    sender.connect(receiver.local_addr()?).await?;
    let mut expected_hash = Sha256::new();
    // Intentionally withhold reads for one finite burst; compare capacity,
    // not scheduling latency or sustained throughput.
    for sequence in 0..COUNT {
        let bytes = packet(sequence);
        expected_hash.update(&bytes);
        ensure!(sender.send(&bytes).await? == bytes.len());
    }
    let mut received = vec![false; COUNT];
    let mut buffer = vec![0u8; PACKET_BYTES + 1];
    while let Ok(result) =
        tokio::time::timeout(Duration::from_millis(50), receiver.recv(&mut buffer)).await
    {
        let length = result?;
        ensure!(length == PACKET_BYTES, "truncated or oversized datagram");
        let sequence = u32::from_be_bytes(buffer[..4].try_into()?) as usize;
        ensure!(
            sequence < COUNT && !received[sequence],
            "invalid or duplicate sequence"
        );
        ensure!(
            buffer[..length] == packet(sequence),
            "datagram content mismatch"
        );
        received[sequence] = true;
    }
    let count = received.iter().filter(|&&present| present).count();
    let mut actual_hash = Sha256::new();
    for (sequence, &present) in received.iter().enumerate() {
        if present {
            actual_hash.update(packet(sequence));
        }
    }
    let expected_hash = hex::encode(expected_hash.finalize());
    let actual_hash = hex::encode(actual_hash.finalize());
    let reports = reports.lock().unwrap();
    ensure!(reports.len() == 1);
    ensure!(
        reports[0].error_stage.is_none(),
        "receive buffer operation failed"
    );
    Ok(json!({
        "buffer":reports[0],"sent_packets":COUNT,"received_packets":count,
        "missing_packets":COUNT-count,"packet_bytes":PACKET_BYTES,
        "received_content_verified":true,"all_received":count==COUNT,
        "expected_sha256":expected_hash,"received_sha256":actual_hash,
        "sha256_match":expected_hash==actual_hash,
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let candidate = network::requested_receive_buffer_bytes(&Value::Null, false);
    let mut results = Vec::new();
    for requested in [0, candidate, candidate, 0, 0, candidate] {
        results.push(burst(requested).await?);
    }
    let verified = results
        .iter()
        .filter(|r| r["buffer"]["requested_bytes"] != 0)
        .all(|r| r["all_received"] == true && r["sha256_match"] == true);
    println!("{}", json!({"verified":verified,"rounds":results}));
    ensure!(
        verified,
        "configured buffer could not retain the complete synthetic burst"
    );
    Ok(())
}
