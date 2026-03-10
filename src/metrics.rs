use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Simple metrics registry using atomics. No external dependencies.
#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

#[derive(Default)]
struct MetricsInner {
    pub calls_total: AtomicU64,
    pub calls_inbound: AtomicU64,
    pub calls_outbound: AtomicU64,
    pub webhooks_sent: AtomicU64,
    pub webhooks_failed: AtomicU64,
    pub http_requests: AtomicU64,
    pub ws_connections: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc_calls_total(&self) {
        self.inner.calls_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_calls_inbound(&self) {
        self.inner.calls_inbound.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_calls_outbound(&self) {
        self.inner.calls_outbound.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_webhooks_sent(&self) {
        self.inner.webhooks_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_webhooks_failed(&self) {
        self.inner.webhooks_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_http_requests(&self) {
        self.inner.http_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_ws_connections(&self) {
        self.inner.ws_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_ws_connections(&self) {
        self.inner.ws_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Render metrics in Prometheus text exposition format.
    pub fn render(&self, active_calls: usize) -> String {
        let i = &self.inner;
        format!(
            "\
# HELP xbridge_calls_total Total calls processed
# TYPE xbridge_calls_total counter
xbridge_calls_total {{direction=\"inbound\"}} {inbound}
xbridge_calls_total {{direction=\"outbound\"}} {outbound}
# HELP xbridge_active_calls Currently active calls
# TYPE xbridge_active_calls gauge
xbridge_active_calls {active}
# HELP xbridge_http_requests_total Total HTTP requests
# TYPE xbridge_http_requests_total counter
xbridge_http_requests_total {http}
# HELP xbridge_ws_connections Active WebSocket connections
# TYPE xbridge_ws_connections gauge
xbridge_ws_connections {ws}
# HELP xbridge_webhooks_total Total webhook deliveries
# TYPE xbridge_webhooks_total counter
xbridge_webhooks_total {{result=\"success\"}} {wh_ok}
xbridge_webhooks_total {{result=\"failure\"}} {wh_fail}
",
            inbound = i.calls_inbound.load(Ordering::Relaxed),
            outbound = i.calls_outbound.load(Ordering::Relaxed),
            active = active_calls,
            http = i.http_requests.load(Ordering::Relaxed),
            ws = i.ws_connections.load(Ordering::Relaxed),
            wh_ok = i.webhooks_sent.load(Ordering::Relaxed),
            wh_fail = i.webhooks_failed.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_and_render() {
        let m = Metrics::new();
        m.inc_calls_inbound();
        m.inc_calls_inbound();
        m.inc_calls_outbound();
        m.inc_webhooks_sent();
        m.inc_webhooks_sent();
        m.inc_webhooks_failed();
        m.inc_http_requests();
        m.inc_ws_connections();

        let output = m.render(3);
        assert!(output.contains("xbridge_calls_total {direction=\"inbound\"} 2"));
        assert!(output.contains("xbridge_calls_total {direction=\"outbound\"} 1"));
        assert!(output.contains("xbridge_active_calls 3"));
        assert!(output.contains("xbridge_http_requests_total 1"));
        assert!(output.contains("xbridge_ws_connections 1"));
        assert!(output.contains("xbridge_webhooks_total {result=\"success\"} 2"));
        assert!(output.contains("xbridge_webhooks_total {result=\"failure\"} 1"));
    }

    #[test]
    fn ws_connections_decrement() {
        let m = Metrics::new();
        m.inc_ws_connections();
        m.inc_ws_connections();
        m.dec_ws_connections();
        let output = m.render(0);
        assert!(output.contains("xbridge_ws_connections 1"));
    }

    #[test]
    fn clone_shares_state() {
        let m1 = Metrics::new();
        let m2 = m1.clone();
        m1.inc_calls_inbound();
        m2.inc_calls_inbound();
        let output = m1.render(0);
        assert!(output.contains("xbridge_calls_total {direction=\"inbound\"} 2"));
    }
}
