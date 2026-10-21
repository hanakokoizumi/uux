use std::io;
use std::net::SocketAddr;

use super::*;
use crate::chunk::chunk_selective_ack::GapAckBlock;

type Result<T> = std::result::Result<T, util::Error>;

impl From<Error> for util::Error {
    fn from(e: Error) -> Self {
        util::Error::from_std(e)
    }
}

struct DumbConn;

#[async_trait]
impl Conn for DumbConn {
    async fn connect(&self, _addr: SocketAddr) -> Result<()> {
        Err(io::Error::other("Not applicable").into())
    }

    async fn recv(&self, _b: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    async fn recv_from(&self, _buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        Err(io::Error::other("Not applicable").into())
    }

    async fn send(&self, _b: &[u8]) -> Result<usize> {
        Ok(0)
    }

    async fn send_to(&self, _buf: &[u8], _target: SocketAddr) -> Result<usize> {
        Err(io::Error::other("Not applicable").into())
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        Err(io::Error::new(io::ErrorKind::AddrNotAvailable, "Addr Not Available").into())
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        None
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

fn create_association_internal(config: Config) -> AssociationInternal {
    let (close_loop_ch_tx, _close_loop_ch_rx) = broadcast::channel(1);
    let (accept_ch_tx, _accept_ch_rx) = mpsc::channel(1);
    let (handshake_completed_ch_tx, _handshake_completed_ch_rx) = mpsc::channel(1);
    let (awake_write_loop_ch_tx, _awake_write_loop_ch_rx) = mpsc::channel(1);
    AssociationInternal::new(
        config,
        close_loop_ch_tx,
        accept_ch_tx,
        handshake_completed_ch_tx,
        Arc::new(awake_write_loop_ch_tx),
    )
}

fn recovery_test_association() -> AssociationInternal {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 1191,
        name: "recovery-boundary-test".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    a.set_state(AssociationState::Established);
    a.use_forward_tsn = false;
    a.cumulative_tsn_ack_point = 100;
    a.my_next_tsn = 113;
    a.cwnd = 16 * a.mtu;
    a.ssthresh = 32 * a.mtu;
    a.rwnd = 65536 - 1200;
    for tsn in 101..=112 {
        a.inflight_queue.push_no_check(ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn,
            nsent: 1,
            user_data: Bytes::from(vec![tsn as u8; 100]),
            ..Default::default()
        });
    }
    a
}

#[tokio::test]
async fn test_fast_recovery_keeps_original_flight_boundary() -> Result<()> {
    let mut a = recovery_test_association();
    for end in 2..=4 {
        a.handle_sack(&ChunkSelectiveAck {
            cumulative_tsn_ack: 100,
            advertised_receiver_window_credit: 65536,
            gap_ack_blocks: vec![GapAckBlock { start: 2, end }],
            ..Default::default()
        })
        .await?;
    }
    assert!(a.in_fast_recovery);
    assert_eq!(
        a.fast_recover_exit_point, 112,
        "HTNA is not the flight boundary"
    );
    assert_eq!(a.cwnd, 8 * a.mtu);
    assert_eq!(
        a.gather_outbound_fast_retransmission_packets(vec![]).len(),
        1
    );
    // Repair of the first hole must not end recovery while later original
    // packets are outstanding, nor halve cwnd again for the same flight.
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 104,
        advertised_receiver_window_credit: 65536,
        ..Default::default()
    })
    .await?;
    assert!(a.in_fast_recovery);
    assert_eq!(a.cwnd, 8 * a.mtu);
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 112,
        advertised_receiver_window_credit: 65536,
        ..Default::default()
    })
    .await?;
    assert!(!a.in_fast_recovery);
    assert!(a.inflight_queue.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_fast_recovery_counts_only_reported_gaps() -> Result<()> {
    let mut a = recovery_test_association();
    a.in_fast_recovery = true;
    a.fast_recover_exit_point = 112;
    // Three partial ACKs repair successive holes near the start. Data beyond
    // the last gap block is still in transit, not reported missing by the peer.
    for cumulative in [101, 103, 105] {
        a.handle_sack(&ChunkSelectiveAck {
            cumulative_tsn_ack: cumulative,
            advertised_receiver_window_credit: 65536,
            gap_ack_blocks: vec![GapAckBlock { start: 2, end: 2 }],
            ..Default::default()
        })
        .await?;
        assert_eq!(
            a.inflight_queue.get(cumulative + 1).unwrap().miss_indicator,
            1
        );
        for tsn in cumulative + 3..=112 {
            assert_eq!(
                a.inflight_queue.get(tsn).unwrap().miss_indicator,
                0,
                "TSN {tsn} lies beyond the peer's reported gap"
            );
        }
        assert!(a
            .gather_outbound_fast_retransmission_packets(vec![])
            .is_empty());
    }
    assert_eq!(a.stats.get_num_fast_retrans(), 0);
    assert_eq!(a.stats.get_num_t3timeouts(), 0);
    Ok(())
}

#[tokio::test]
async fn test_fast_recovery_tracks_new_gap_acks_before_cumulative_advance() -> Result<()> {
    let mut a = recovery_test_association();
    for end in 2..=4 {
        a.handle_sack(&ChunkSelectiveAck {
            cumulative_tsn_ack: 100,
            advertised_receiver_window_credit: 65536,
            gap_ack_blocks: vec![GapAckBlock { start: 2, end }],
            ..Default::default()
        })
        .await?;
    }
    // First hole (101) has been retransmitted; its ACK has not arrived yet.
    let first = a.gather_outbound_fast_retransmission_packets(vec![]);
    assert_eq!(first.len(), 1);
    assert_eq!(a.inflight_queue.get(101).unwrap().nsent, 2);
    // New gap ACKs for 106..108 report a second hole (105) three times,
    // despite the unchanged cumulative ACK. HTNA must still count these.
    for end in 6..=8 {
        a.handle_sack(&ChunkSelectiveAck {
            cumulative_tsn_ack: 100,
            advertised_receiver_window_credit: 65536,
            gap_ack_blocks: vec![
                GapAckBlock { start: 2, end: 4 },
                GapAckBlock { start: 6, end },
            ],
            ..Default::default()
        })
        .await?;
        assert_eq!(
            a.inflight_queue.get(105).unwrap().miss_indicator,
            u32::from(end - 5)
        );
        if end == 6 {
            a.handle_sack(&ChunkSelectiveAck {
                cumulative_tsn_ack: 100,
                advertised_receiver_window_credit: 65536,
                gap_ack_blocks: vec![
                    GapAckBlock { start: 2, end: 4 },
                    GapAckBlock { start: 6, end: 6 },
                ],
                ..Default::default()
            })
            .await?;
            assert_eq!(
                a.inflight_queue.get(105).unwrap().miss_indicator,
                1,
                "a duplicate SACK must not add another missing indication"
            );
        }
    }
    // An identical SACK is not a fourth missing report.
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 100,
        advertised_receiver_window_credit: 65536,
        gap_ack_blocks: vec![
            GapAckBlock { start: 2, end: 4 },
            GapAckBlock { start: 6, end: 8 },
        ],
        ..Default::default()
    })
    .await?;
    assert_eq!(a.inflight_queue.get(105).unwrap().miss_indicator, 3);
    // Repair of the first hole now lets the existing recovery scheduler send
    // the second hole immediately, without waiting for a retransmission timer.
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 104,
        advertised_receiver_window_credit: 65536,
        gap_ack_blocks: vec![GapAckBlock { start: 2, end: 4 }],
        ..Default::default()
    })
    .await?;
    let next = a.gather_outbound_fast_retransmission_packets(vec![]);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].chunks.len(), 1);
    let data = next[0].chunks[0]
        .as_any()
        .downcast_ref::<ChunkPayloadData>()
        .unwrap();
    assert_eq!(data.tsn, 105);
    assert_eq!(data.user_data.as_ref(), &[105; 100]);
    assert_eq!(
        a.cwnd,
        8 * a.mtu,
        "one recovery must not repeatedly reduce cwnd"
    );
    assert_eq!(a.stats.get_num_fast_retrans(), 2);
    assert_eq!(a.stats.get_num_t3timeouts(), 0);
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 112,
        advertised_receiver_window_credit: 65536,
        ..Default::default()
    })
    .await?;
    assert!(!a.in_fast_recovery);
    assert!(a.inflight_queue.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_gap_sack_wakes_sender_without_forward_tsn() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 1191,
        name: "gap-wakeup-test".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    let (tx, mut rx) = mpsc::channel(1);
    a.awake_write_loop_ch = Arc::new(tx);
    a.set_state(AssociationState::Established);
    a.use_forward_tsn = false;
    a.cumulative_tsn_ack_point = 100;
    a.my_next_tsn = 105;
    a.rwnd = 65536 - 4 * 100;
    for tsn in 101..=104 {
        a.inflight_queue.push_no_check(ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn,
            nsent: 1,
            user_data: Bytes::from(vec![tsn as u8; 100]),
            ..Default::default()
        });
    }
    for end in 2..=4 {
        a.handle_sack(&ChunkSelectiveAck {
            cumulative_tsn_ack: 100,
            advertised_receiver_window_credit: 65536,
            gap_ack_blocks: vec![GapAckBlock { start: 2, end }],
            ..Default::default()
        })
        .await?;
        assert!(rx.try_recv().is_ok(), "new gap ACK must wake the sender");
        assert_eq!(a.cumulative_tsn_ack_point, 100);
        assert_eq!(a.will_retransmit_fast, end == 4);
    }
    let packets = a.gather_outbound_fast_retransmission_packets(vec![]);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].chunks.len(), 1);
    let retransmission = packets[0].chunks[0]
        .as_any()
        .downcast_ref::<ChunkPayloadData>()
        .unwrap();
    assert_eq!(retransmission.tsn, 101);
    assert_eq!(retransmission.user_data.as_ref(), &[101; 100]);
    assert_eq!(a.stats.get_num_fast_retrans(), 1);
    assert_eq!(a.stats.get_num_t3timeouts(), 0);

    // An unchanged duplicate SACK must not create a busy sender loop.
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 100,
        advertised_receiver_window_credit: 65536,
        gap_ack_blocks: vec![GapAckBlock { start: 2, end: 4 }],
        ..Default::default()
    })
    .await?;
    assert!(rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn test_sack_reopened_window_wakes_sender_without_forward_tsn() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 1191,
        name: "window-wakeup-test".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    let (tx, mut rx) = mpsc::channel(1);
    a.awake_write_loop_ch = Arc::new(tx);
    a.set_state(AssociationState::Established);
    a.use_forward_tsn = false;
    a.cumulative_tsn_ack_point = 100;
    a.rwnd = 0;
    a.handle_sack(&ChunkSelectiveAck {
        cumulative_tsn_ack: 100,
        advertised_receiver_window_credit: 65536,
        ..Default::default()
    })
    .await?;
    assert_eq!(a.rwnd, 65536);
    assert!(
        rx.try_recv().is_ok(),
        "reopened receive window must wake the sender"
    );
    Ok(())
}

#[test]
fn test_config_mtu() -> Result<()> {
    // 0 means the default; a nonzero value is used as given and
    // bounds the DATA chunk payload size.
    let a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    assert_eq!(a.mtu, INITIAL_MTU);
    assert_eq!(
        a.max_payload_size,
        (INITIAL_MTU - (COMMON_HEADER_SIZE + DATA_CHUNK_HEADER_SIZE)) & !3
    );

    let a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 8192,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    assert_eq!(a.mtu, 8192);
    assert_eq!(
        a.max_payload_size,
        8192 - (COMMON_HEADER_SIZE + DATA_CHUNK_HEADER_SIZE)
    );

    Ok(())
}

#[test]
fn test_bundled_data_respects_mtu_with_headers_and_padding() -> Result<()> {
    for mtu in [576, 1191, 1200, 8192] {
        let a = create_association_internal(Config {
            net_conn: Arc::new(DumbConn {}),
            max_receive_buffer_size: 0,
            max_message_size: 0,
            mtu,
            name: "mtu-test".to_owned(),
            local_port: 5000,
            remote_port: 5000,
        });
        for lengths in [
            vec![a.max_payload_size as usize; 3],
            vec![1; 80],
            vec![217; 10],
            vec![a.max_payload_size as usize - 20, 1, 2, 3, 4, 19, 20],
        ] {
            let chunks = lengths
                .iter()
                .enumerate()
                .map(|(i, &len)| ChunkPayloadData {
                    beginning_fragment: true,
                    ending_fragment: true,
                    tsn: i as u32,
                    stream_identifier: 1,
                    stream_sequence_number: i as u16,
                    user_data: Bytes::from(vec![i as u8; len]),
                    ..Default::default()
                })
                .collect();
            let packets = a.bundle_data_chunks_into_packets(chunks);
            let mut seen = 0;
            for packet in packets {
                assert!(!packet.chunks.is_empty(), "empty DATA packet");
                let wire = packet.marshal()?;
                assert!(
                    wire.len() <= mtu as usize,
                    "wire size {} exceeds MTU {mtu}",
                    wire.len()
                );
                let decoded = Packet::unmarshal(&wire)?;
                for chunk in decoded.chunks {
                    let data = chunk.as_any().downcast_ref::<ChunkPayloadData>().unwrap();
                    assert_eq!(data.tsn, seen as u32);
                    assert_eq!(data.user_data.as_ref(), vec![seen as u8; lengths[seen]]);
                    seen += 1;
                }
            }
            assert_eq!(seen, lengths.len());
        }
    }
    Ok(())
}

#[test]
fn test_fast_retransmit_accounts_for_chunk_padding() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 1191,
        name: "mtu-test".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    a.cumulative_tsn_ack_point = 100;
    a.will_retransmit_fast = true;
    for tsn in 101..=105 {
        a.inflight_queue.push_no_check(ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn,
            user_data: Bytes::from(vec![0; 217]),
            miss_indicator: 3,
            nsent: 1,
            ..Default::default()
        });
    }
    let packets = a.gather_outbound_fast_retransmission_packets(vec![]);
    assert_eq!(packets.len(), 1);
    let wire = packets[0].marshal()?;
    assert!(
        wire.len() <= a.mtu as usize,
        "fast retransmit exceeds MTU: {}",
        wire.len()
    );
    assert_eq!(packets[0].chunks.len(), 4);
    assert_eq!(a.inflight_queue.get(105).unwrap().nsent, 1);
    Ok(())
}

#[test]
fn test_create_forward_tsn_forward_one_abandoned() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });

    a.cumulative_tsn_ack_point = 9;
    a.advanced_peer_tsn_ack_point = 10;
    a.inflight_queue.push_no_check(ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 10,
        stream_identifier: 1,
        stream_sequence_number: 2,
        user_data: Bytes::from_static(b"ABC"),
        nsent: 1,
        abandoned: Arc::new(AtomicBool::new(true)),
        ..Default::default()
    });

    let fwdtsn = a.create_forward_tsn();

    assert_eq!(fwdtsn.new_cumulative_tsn, 10, "should be able to serialize");
    assert_eq!(fwdtsn.streams.len(), 1, "there should be one stream");
    assert_eq!(fwdtsn.streams[0].identifier, 1, "si should be 1");
    assert_eq!(fwdtsn.streams[0].sequence, 2, "ssn should be 2");

    Ok(())
}

#[test]
fn test_create_forward_tsn_forward_two_abandoned_with_the_same_si() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });

    a.cumulative_tsn_ack_point = 9;
    a.advanced_peer_tsn_ack_point = 12;
    a.inflight_queue.push_no_check(ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 10,
        stream_identifier: 1,
        stream_sequence_number: 2,
        user_data: Bytes::from_static(b"ABC"),
        nsent: 1,
        abandoned: Arc::new(AtomicBool::new(true)),
        ..Default::default()
    });
    a.inflight_queue.push_no_check(ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 11,
        stream_identifier: 1,
        stream_sequence_number: 3,
        user_data: Bytes::from_static(b"DEF"),
        nsent: 1,
        abandoned: Arc::new(AtomicBool::new(true)),
        ..Default::default()
    });
    a.inflight_queue.push_no_check(ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 12,
        stream_identifier: 2,
        stream_sequence_number: 1,
        user_data: Bytes::from_static(b"123"),
        nsent: 1,
        abandoned: Arc::new(AtomicBool::new(true)),
        ..Default::default()
    });

    let fwdtsn = a.create_forward_tsn();

    assert_eq!(fwdtsn.new_cumulative_tsn, 12, "should be able to serialize");
    assert_eq!(fwdtsn.streams.len(), 2, "there should be two stream");

    let mut si1ok = false;
    let mut si2ok = false;
    for s in &fwdtsn.streams {
        match s.identifier {
            1 => {
                assert_eq!(3, s.sequence, "ssn should be 3");
                si1ok = true;
            }
            2 => {
                assert_eq!(1, s.sequence, "ssn should be 1");
                si2ok = true;
            }
            _ => panic!("unexpected stream identifier"),
        }
    }
    assert!(si1ok, "si=1 should be present");
    assert!(si2ok, "si=2 should be present");

    Ok(())
}

#[tokio::test]
async fn test_handle_forward_tsn_forward_3unreceived_chunks() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    a.use_forward_tsn = true;

    let prev_tsn = a.peer_last_tsn;

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn + 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 0,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn).await?;

    let delayed_ack_triggered = a.delayed_ack_triggered;
    let immediate_ack_triggered = a.immediate_ack_triggered;
    assert_eq!(
        a.peer_last_tsn,
        prev_tsn + 3,
        "peerLastTSN should advance by 3 "
    );
    assert!(delayed_ack_triggered, "delayed sack should be triggered");
    assert!(
        !immediate_ack_triggered,
        "immediate sack should NOT be triggered"
    );
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[tokio::test]
async fn test_handle_forward_tsn_forward_1for1_missing() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    a.use_forward_tsn = true;

    let prev_tsn = a.peer_last_tsn;

    // this chunk is blocked by the missing chunk at tsn=1
    a.payload_queue.push(
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: a.peer_last_tsn + 2,
            stream_identifier: 0,
            stream_sequence_number: 1,
            user_data: Bytes::from_static(b"ABC"),
            ..Default::default()
        },
        a.peer_last_tsn,
    );

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn + 1,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 1,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn).await?;

    let delayed_ack_triggered = a.delayed_ack_triggered;
    let immediate_ack_triggered = a.immediate_ack_triggered;
    assert_eq!(
        a.peer_last_tsn,
        prev_tsn + 2,
        "peerLastTSN should advance by 2"
    );
    assert!(delayed_ack_triggered, "delayed sack should be triggered");
    assert!(
        !immediate_ack_triggered,
        "immediate sack should NOT be triggered"
    );
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[tokio::test]
async fn test_handle_forward_tsn_forward_1for2_missing() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    a.use_forward_tsn = true;

    let prev_tsn = a.peer_last_tsn;

    // this chunk is blocked by the missing chunk at tsn=1
    a.payload_queue.push(
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: a.peer_last_tsn + 3,
            stream_identifier: 0,
            stream_sequence_number: 1,
            user_data: Bytes::from_static(b"ABC"),
            ..Default::default()
        },
        a.peer_last_tsn,
    );

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn + 1,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 1,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn).await?;

    let immediate_ack_triggered = a.immediate_ack_triggered;
    assert_eq!(
        a.peer_last_tsn,
        prev_tsn + 1,
        "peerLastTSN should advance by 1"
    );
    assert!(
        immediate_ack_triggered,
        "immediate sack should be triggered"
    );
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[tokio::test]
async fn test_handle_forward_tsn_dup_forward_tsn_chunk_should_generate_sack() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    a.use_forward_tsn = true;

    let prev_tsn = a.peer_last_tsn;

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 1,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn).await?;

    assert_eq!(a.peer_last_tsn, prev_tsn, "peerLastTSN should not advance");
    assert_eq!(a.ack_state, AckState::Immediate, "sack should be requested");
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[tokio::test]
async fn test_assoc_create_new_stream() -> Result<()> {
    let (close_loop_ch_tx, _close_loop_ch_rx) = broadcast::channel(1);
    let (accept_ch_tx, _accept_ch_rx) = mpsc::channel(ACCEPT_CH_SIZE);
    let (handshake_completed_ch_tx, _handshake_completed_ch_rx) = mpsc::channel(1);
    let (awake_write_loop_ch_tx, _awake_write_loop_ch_rx) = mpsc::channel(1);
    let mut a = AssociationInternal::new(
        Config {
            net_conn: Arc::new(DumbConn {}),
            max_receive_buffer_size: 0,
            max_message_size: 0,
            mtu: 0,
            name: "client".to_owned(),
            local_port: 5000,
            remote_port: 5000,
        },
        close_loop_ch_tx,
        accept_ch_tx,
        handshake_completed_ch_tx,
        Arc::new(awake_write_loop_ch_tx),
    );

    for i in 0..ACCEPT_CH_SIZE {
        let s = a.create_stream(i as u16, true);
        if let Some(s) = s {
            let result = a.streams.get(&s.stream_identifier);
            assert!(result.is_some(), "should be in a.streams map");
        } else {
            panic!("{i} should success");
        }
    }

    let new_si = ACCEPT_CH_SIZE as u16;
    let s = a.create_stream(new_si, true);
    assert!(s.is_none(), "should be none");
    let result = a.streams.get(&new_si);
    assert!(result.is_none(), "should NOT be in a.streams map");

    let to_be_ignored = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: new_si,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let p = a.handle_data(&to_be_ignored).await?;
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

async fn handle_init_test(name: &str, initial_state: AssociationState, expect_err: bool) {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5002,
        remote_port: 5001,
    });
    a.set_state(initial_state);
    let pkt = Packet {
        source_port: 5001,
        destination_port: 5002,
        ..Default::default()
    };
    let mut init = ChunkInit {
        initial_tsn: 1234,
        num_outbound_streams: 1001,
        num_inbound_streams: 1002,
        initiate_tag: 5678,
        advertised_receiver_window_credit: 512 * 1024,
        ..Default::default()
    };
    init.set_supported_extensions();

    let result = a.handle_init(&pkt, &init).await;
    if expect_err {
        assert!(result.is_err(), "{name} should fail");
        return;
    } else {
        assert!(result.is_ok(), "{name} should be ok");
    }
    assert_eq!(
        a.peer_last_tsn,
        if init.initial_tsn == 0 {
            u32::MAX
        } else {
            init.initial_tsn - 1
        },
        "{name} should match"
    );
    assert_eq!(a.my_max_num_outbound_streams, 1001, "{name} should match");
    assert_eq!(a.my_max_num_inbound_streams, 1002, "{name} should match");
    assert_eq!(a.peer_verification_tag, 5678, "{name} should match");
    assert_eq!(a.destination_port, pkt.source_port, "{name} should match");
    assert_eq!(a.source_port, pkt.destination_port, "{name} should match");
    assert!(a.use_forward_tsn, "{name} should be set to true");
}

#[tokio::test]
async fn test_assoc_handle_init() -> Result<()> {
    handle_init_test("normal", AssociationState::Closed, false).await;

    handle_init_test(
        "unexpected state established",
        AssociationState::Established,
        true,
    )
    .await;

    handle_init_test(
        "unexpected state shutdownAckSent",
        AssociationState::ShutdownAckSent,
        true,
    )
    .await;

    handle_init_test(
        "unexpected state shutdownPending",
        AssociationState::ShutdownPending,
        true,
    )
    .await;

    handle_init_test(
        "unexpected state shutdownReceived",
        AssociationState::ShutdownReceived,
        true,
    )
    .await;

    handle_init_test(
        "unexpected state shutdownSent",
        AssociationState::ShutdownSent,
        true,
    )
    .await;

    Ok(())
}

#[tokio::test]
async fn test_assoc_max_message_size_default() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });
    assert_eq!(
        a.max_message_size.load(Ordering::SeqCst),
        65536,
        "should match"
    );

    let stream = a.create_stream(1, false);
    assert!(stream.is_some(), "should succeed");

    if let Some(s) = stream {
        let p = Bytes::from(vec![0u8; 65537]);
        let ppi = PayloadProtocolIdentifier::from(s.default_payload_type.load(Ordering::SeqCst));

        if let Err(err) = s.write_sctp(&p.slice(..65536), ppi).await {
            assert_ne!(
                err,
                Error::ErrOutboundPacketTooLarge,
                "should be not Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }

        if let Err(err) = s.write_sctp(&p.slice(..65537), ppi).await {
            assert_eq!(
                err,
                Error::ErrOutboundPacketTooLarge,
                "should be Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_assoc_max_message_size_explicit() -> Result<()> {
    let mut a = create_association_internal(Config {
        net_conn: Arc::new(DumbConn {}),
        max_receive_buffer_size: 0,
        max_message_size: 30000,
        mtu: 0,
        name: "client".to_owned(),
        local_port: 5000,
        remote_port: 5000,
    });

    assert_eq!(
        a.max_message_size.load(Ordering::SeqCst),
        30000,
        "should match"
    );

    let stream = a.create_stream(1, false);
    assert!(stream.is_some(), "should succeed");

    if let Some(s) = stream {
        let p = Bytes::from(vec![0u8; 30001]);
        let ppi = PayloadProtocolIdentifier::from(s.default_payload_type.load(Ordering::SeqCst));

        if let Err(err) = s.write_sctp(&p.slice(..30000), ppi).await {
            assert_ne!(
                err,
                Error::ErrOutboundPacketTooLarge,
                "should be not Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }

        if let Err(err) = s.write_sctp(&p.slice(..30001), ppi).await {
            assert_eq!(
                err,
                Error::ErrOutboundPacketTooLarge,
                "should be Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }
    }

    Ok(())
}
