use serde::Serialize;
use serde_json::Value;
use socket2::SockRef;
use tokio::net::UdpSocket;
use webrtc::util::vnet::net::Net;

const DEFAULT_RECEIVE_BUFFER_BYTES: usize = 2 * 1024 * 1024;

pub fn requested_receive_buffer_bytes(settings: &Value, diagnostics: bool) -> usize {
    // Zero keeps the OS default for controlled comparisons. Do not expose a
    // transport experiment through persisted user preferences.
    if diagnostics
        && let Some(kib) = settings["udp_receive_buffer_kib"]
            .as_u64()
            .filter(|kib| *kib <= 4096)
    {
        return kib as usize * 1024;
    }
    DEFAULT_RECEIVE_BUFFER_BYTES
}

#[derive(Clone, Debug, Serialize)]
pub struct ReceiveBuffer {
    pub requested_bytes: usize,
    pub before_bytes: Option<usize>,
    pub after_bytes: Option<usize>,
    pub attempted: bool,
    pub error_stage: Option<&'static str>,
    pub errno: Option<i32>,
}

pub fn configure_receive_buffer(socket: &UdpSocket, requested_bytes: usize) -> ReceiveBuffer {
    let socket = SockRef::from(socket);
    let mut result = ReceiveBuffer {
        requested_bytes,
        before_bytes: None,
        after_bytes: None,
        attempted: false,
        error_stage: None,
        errno: None,
    };
    let before = match socket.recv_buffer_size() {
        Ok(size) => size,
        Err(error) => {
            result.error_stage = Some("query_before");
            result.errno = error.raw_os_error();
            return result;
        }
    };
    result.before_bytes = Some(before);
    result.after_bytes = Some(before);
    // Linux reports twice the requested SO_RCVBUF for kernel accounting.
    // Leave an already larger buffer intact. Use SO_RCVBUF, never FORCE or a
    // global sysctl; the OS may clamp the request, so record the actual result.
    let target = if cfg!(target_os = "linux") {
        requested_bytes.saturating_mul(2)
    } else {
        requested_bytes
    };
    if requested_bytes == 0 || before >= target {
        return result;
    }
    result.attempted = true;
    if let Err(error) = socket.set_recv_buffer_size(requested_bytes) {
        result.error_stage = Some("set");
        result.errno = error.raw_os_error();
        return result;
    }
    match socket.recv_buffer_size() {
        Ok(size) => result.after_bytes = Some(size),
        Err(error) => {
            result.after_bytes = None;
            result.error_stage = Some("query_after");
            result.errno = error.raw_os_error();
        }
    }
    result
}

pub fn system_network(
    requested_bytes: usize,
    report: impl Fn(ReceiveBuffer) + Send + Sync + 'static,
) -> Net {
    Net::new(None).with_udp_socket_hook(move |socket| {
        report(configure_receive_buffer(socket, requested_bytes));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn receive_buffer_override_is_bounded_and_diagnostic_only() {
        assert_eq!(
            requested_receive_buffer_bytes(&json!({}), true),
            2 * 1024 * 1024
        );
        assert_eq!(
            requested_receive_buffer_bytes(&json!({"udp_receive_buffer_kib":0}), true),
            0
        );
        for value in [json!(-1), json!(4097), json!("2048"), Value::Null] {
            assert_eq!(
                requested_receive_buffer_bytes(&json!({"udp_receive_buffer_kib":value}), true),
                DEFAULT_RECEIVE_BUFFER_BYTES
            );
        }
        assert_eq!(
            requested_receive_buffer_bytes(&json!({"udp_receive_buffer_kib":0}), false),
            DEFAULT_RECEIVE_BUFFER_BYTES
        );
    }

    #[tokio::test]
    async fn receive_buffer_preserves_default_and_never_shrinks() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let initial = SockRef::from(&socket).recv_buffer_size().unwrap();
        for requested in [0, 1] {
            let report = configure_receive_buffer(&socket, requested);
            assert!(!report.attempted);
            assert_eq!(report.after_bytes, Some(initial));
            assert_eq!(report.error_stage, None);
            assert_eq!(SockRef::from(&socket).recv_buffer_size().unwrap(), initial);
        }
    }

    #[tokio::test]
    async fn receive_buffer_reports_actual_kernel_result() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let report = configure_receive_buffer(&socket, DEFAULT_RECEIVE_BUFFER_BYTES);
        assert_eq!(report.error_stage, None);
        assert_eq!(
            report.after_bytes,
            Some(SockRef::from(&socket).recv_buffer_size().unwrap())
        );
        // A low system limit may clamp this; do not assume the request succeeded
        // at full size on every host.
        assert!(report.after_bytes.unwrap() >= report.before_bytes.unwrap());
    }
}
