# UU native controller transport patches

Upstream: `webrtc-sctp` 0.17.2, copied from the Cargo registry package. The original MIT/Apache licenses are retained. `.cargo*` registry metadata and the nested lockfile are omitted. This folder does not depend on Wine or Python.

Duplicate-TSN reporting transfers ownership of its `Vec` using `mem::take`, instead of draining into another allocation. Reported values and queue-emptying semantics are unchanged; this also satisfies the current Clippy `drain_collect` check.

Timing changes in `src/timer/rtx_timer.rs` and its existing timer tests:

- Reduce the measured steady-state minimum RTO from 1000 to 400 ms.
- Keep at least 220 ms of delayed-ACK headroom in addition to measured SRTT.
- Add regression coverage for low RTT, slower links, spikes and backoff; update the existing 600 ms RTT expectation to follow the new floor.

Rationale: sparse reliable input cannot count on three later SACKs to trigger fast retransmit. Two consecutive losses in our deterministic local interoperability test cost 3003 ms before this patch. This is a recovery-time change, not a cure for packet loss.

The 400 ms floor and 220 ms delayed-ACK allowance follow the values in [Google's dcSCTP options](https://webrtc.googlesource.com/src/+/d68f18ee6e52d572e09a2f70d811f45b3429de95/net/dcsctp/public/dcsctp_options.h). The variance contribution is floored in the RTO calculation; the existing estimator itself is retained. Initial 3000 ms handshake timing, 60000 ms maximum, exponential backoff, congestion control, ordered reliable delivery, checksum verification and DTLS behavior remain upstream defaults.

Packet-size corrections in `src/association/association_internal.rs`:

- Round the maximum fragment payload down to a four-byte boundary so padding fits even when the MTU is not divisible by four.
- Count both the DATA header and alignment padding when bundling normal sends and fast retransmissions. Avoid emitting empty packets when starting a bundle.
- Add regression tests that serialize and parse mixed-size payloads, checking packet limits, content, order, and that chunks which do not fit a fast retransmission remain unsent.

Before correction, the normal-send test produced 692 bytes with an MTU of 576; the fast-retransmission test produced 1192 bytes with an MTU of 1191. [RFC 9260 sections 6.9–6.10](https://www.rfc-editor.org/rfc/rfc9260.html#section-6.10) require packet sizing to include headers and padding. This fixes a reproducible framing error; it does not establish that oversized packets caused all live-session stalls.

The SACK handler also emits an opt-in trace with only gap-block counts, acknowledged chunk counts, highest relative offset and duplicate counts. The application reduces these to numeric gap diagnostics; it does not log TSNs, addresses or payload. Normal runs leave this trace disabled.

Validation from the parent project: `cargo test -p webrtc-sctp --lib -- --skip fuzz_artifact_test`; `cargo build --release --example transport_probe`; run `../tools/transport_probe.py --binary ./target/release/examples/transport_probe --drop-small 2` with the development Python environment. The injector drops only one designated synthetic message, twice, on the local aiortc test instance. Never use it on a live UU connection.

Upstream test packaging limitations: standalone examples require extra Tokio signal features, so dependency verification targets its library suite. The published registry package omits `fuzz/artifacts/param` and `fuzz/artifacts/packet`; the two corresponding tests fail with `NotFound`. They are explicitly excluded, not replaced with empty folders or counted as passing. The remaining 126 library tests cover retransmission timing, packet sizing, congestion control, reassembly and association behavior.

When upgrading WebRTC, reassess and remove this local patch if equivalent per-association timing configuration becomes available. Do not edit the user's global Cargo registry to tune the application.

Timer cancellation correction in `src/timer/rtx_timer.rs`:

- An expired task waiting for the association/observer lock now also listens for cancellation and checks its own channel before invoking the callback.
- A finishing failure callback only clears the registered timer if its own cancellation channel is still open. A stop/start during that callback cannot erase a newer registration.
- Three paused-clock regression tests cover stale callbacks blocked on the observer, stale failure callbacks, and restart while a failure callback is already running. Before the correction, the first two deterministically fired a cancelled callback and cleared a newly started timer, respectively. The new timer must remain active and fire normally.

This preserves RTO values, backoff, maximum retries and congestion rules. It prevents an obsolete timeout from applying those rules to a newer transmission; it does not prove that every live retransmission was spurious. Two exact opt-in trace messages report prevented stale callbacks/cleanup, without payload or connection identifiers. Tokio's `test-util` is a development-only dependency for deterministic paused-clock tests. The runnable library suite now has 129 passing tests, with the same two missing upstream fuzz fixtures explicitly excluded.

SACK wakeup correction in `src/association/association_internal.rs`:

- Wake the output loop when a SACK newly acknowledges gap chunks, schedules a fast retransmission, or increases the usable receive window, even if the cumulative ACK has not advanced.
- This must work without the optional Forward-TSN extension. Previously the wakeup for these cases depended on the extension branch, so a reliable sender could wait for an unrelated event or timer despite having enough information to make progress.
- Two regressions first failed against the old implementation, then passed with the fix. They cover three successive gap ACKs and actual retransmission of the missing payload without a T3 timeout, reopening the receive window, and no spurious wakeup on an unchanged duplicate SACK.

This follows the prompt retransmission requirement in [RFC 9260 section 7.2.4](https://www.rfc-editor.org/rfc/rfc9260.html#section-7.2.4) and does not change congestion/flow-control limits, reliability, ordering, or RTO values. The runnable library suite now has 131 passing tests; the same two missing fuzz fixtures remain explicitly excluded. Independent interop testing uses `../tools/transport_probe.py --drop-bulk --no-forward-tsn`, which disables partial reliability and drops a single designated bulk fragment only on the local aiortc test peer.

Optional `webrtc_sctp::diagnostics` trace metadata:

- Relative DATA indices are computed from the association's initial TSN; the initial/random TSN is never included in this trace. Packet indices start at zero within the writer.
- Successful `net_conn.send` calls report serialized bytes, serialization duration and transport submission duration, plus each DATA chunk's relative index/attempt/size/stream/fragment flags. This is not a packet capture or a measurement of physical network departure. A failed submission has only a fixed event and packet index.
- First acknowledgements and peer duplicate reports reference only relative indices. No payload, peer name, address or arbitrary error text is included.
- The application enables this target only with `UU_SCTP_DETAIL=1` in addition to its existing opt-in logger. Normal runs do not collect timing samples or allocate detail records. Reliability and scheduling are unchanged.

Bounded pending-queue progress correction in `src/queue/pending_queue.rs` and `src/stream/mod.rs`:

- A fragmented message larger than the production 128 KiB queue previously waited for capacity before `append` returned, but the stream woke the output loop only after that return. On an idle association this could wait indefinitely for its own completion.
- Pass the writer notification into `append`. Large appends notify when the next fragment needs capacity, then wait for permits; completed appends still notify normally. Notifications are coalesced by the existing channel, with no polling timer or per-fragment wakeup on the unblocked path.
- Keep the existing byte limit, semaphore backpressure, and whole-message enqueue lock. Fragments of different messages cannot interleave. Update byte/chunk counters under the same queue lock that publishes fragments, so a concurrently running consumer cannot decrement an unaccounted fragment.
- Derive the large-append threshold from the queue instance's capacity. A new paused-clock regression uses the real 128 KiB capacity, instead of the older tests' 128 MiB default, and failed before the fix. It covers both ordered and unordered 200 KiB messages. A second multithreaded test checks two producers, eight large messages, every payload byte, fragment boundaries, bounded accounting, and full draining.

The runnable library suite has 133 passing tests, still excluding only the two missing upstream fuzz fixtures. Independent interoperability uses `../tools/transport_probe.py --idle-large`, with explicit larger-message negotiation on that local test peer. This is a queue liveness fix, not evidence that it caused all live stalls; production file blocks remain 64 KiB. RTO, congestion/window limits, message cancellation semantics, and reliability remain unchanged.

Fast-recovery bookkeeping correction in `src/association/association_internal.rs`:

- Set the recovery exit point to the last TSN already assigned to the original flight, rather than the highest newly acknowledged TSN in the current SACK. A partial repair must not prematurely end recovery and allow another reduction for that same flight.
- When a partial cumulative ACK widens the missing-report range during recovery, stop at the last reported gap block. Later data still in transit is not automatically another loss indication.
- Continue applying HTNA to SACKs containing newly acknowledged chunks even while the cumulative ACK is stuck in recovery. Previously those reports were discarded; later known holes then waited for T3 instead of becoming eligible for the existing recovery scheduler. Unchanged duplicate SACKs add no indication, including below the three-report threshold.

These changes follow the bookkeeping rules in [RFC 9260 section 7.2.4](https://www.rfc-editor.org/rfc/rfc9260.html#section-7.2.4). Three regressions first failed and now pass, checking the flight boundary, no false missing reports beyond the peer's gaps, and recovery of a second hole without a timeout or repeated cwnd reduction. The runnable library suite has 136 passing tests, with the same two unavailable fuzz inputs excluded. Existing timer values, congestion formulas, retransmission scheduling, packet limits, and reliability are retained; this is not a claim of a complete SCTP conformance audit.

`../tools/transport_probe.py --drop-bulk-offsets 0,4,12` drops exactly those fragment offsets once in one synthetic 64 KiB message, only in the local aiortc test instance. It verifies all 8 MiB of bulk content plus 32 small messages. The slowest block's hash acknowledgement decreased from about 404 ms to 3.6 ms in repeated runs. Seven separated losses without Forward-TSN similarly changed from 404.6 ms to 4.2 ms. A fragment withheld repeatedly for 650 ms still follows the existing timeout/backoff behavior; the patch does not make that network condition disappear.
