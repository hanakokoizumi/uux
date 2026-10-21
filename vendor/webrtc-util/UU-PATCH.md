# Local UDP socket configuration hook

Based on the published `webrtc-util 0.17.2` crate; original MIT and Apache-2.0
licenses are retained. Only this workspace uses the Cargo patch. The global
Cargo registry is not modified.

`Net::with_udp_socket_hook` configures OS sockets immediately after `bind`,
before `dial` connects or either method exposes the socket to a consumer. The
hook belongs to one `Net`, applies to both OS socket creation paths, and does
not change virtual networks, interface discovery/filtering, address lookup,
ephemeral port selection, or the underlying `Conn` implementation. Existing
`Net::new(None)` callers retain their original behavior without a hook.

The application passes a non-virtual `Net` through the existing SettingEngine
API. ICE host/srflx collection and UDP TURN still use their original socket
creation paths. It does not enable UDP mux or change candidate types. Buffer
size policy, best-effort failure handling, and numeric diagnostics live in
`src/core/network.rs`, outside this vendor crate.

Two new tests cover both OS socket paths with a real bidirectional loopback
exchange, original interfaces, and preservation of the virtual network. The
89 library tests pass. One existing `conn_map::find` branch was shortened to
`conns.first().map(Arc::clone)` with equivalent behavior, to satisfy the current
strict Clippy check after making the crate a workspace member.

The local `udp_burst_probe` example uses the same application socket factory
to measure a finite burst while reads are deliberately withheld. This tests
buffer capacity, not remote frame rate or end-to-end input latency. Actual
remote validation and limitations are recorded in
[performance notes](../../../docs/Performance-Optimization.md).
