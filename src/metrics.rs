use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Simple metrics registry using atomics. No external dependencies.
#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    calls_inbound: AtomicU64,
    calls_outbound: AtomicU64,
    webhooks_sent: AtomicU64,
    webhooks_failed: AtomicU64,
    http_requests: AtomicU64,
    ws_connections: AtomicU64,
    trunk_calls_inbound: AtomicU64,
    rate_limit_rejections: AtomicU64,
    ws_frames_sent: AtomicU64,
    ws_frames_received: AtomicU64,
    call_duration: Histogram,
    http_request_duration: Histogram,
    webhook_duration: Histogram,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
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

    pub fn inc_trunk_calls_inbound(&self) {
        self.inner
            .trunk_calls_inbound
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rate_limit_rejections(&self) {
        self.inner
            .rate_limit_rejections
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_ws_frames_sent(&self) {
        self.inner.ws_frames_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_ws_frames_received(&self) {
        self.inner
            .ws_frames_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_call_duration(&self, secs: f64) {
        self.inner.call_duration.observe(secs);
    }

    pub fn observe_http_request_duration(&self, secs: f64) {
        self.inner.http_request_duration.observe(secs);
    }

    pub fn observe_webhook_duration(&self, secs: f64) {
        self.inner.webhook_duration.observe(secs);
    }

    /// Render metrics in Prometheus text exposition format.
    pub fn render(&self, active_calls: usize) -> String {
        let i = &self.inner;
        let mut out = format!(
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
# HELP xbridge_ws_frames_total WebSocket frames processed
# TYPE xbridge_ws_frames_total counter
xbridge_ws_frames_total {{direction=\"sent\"}} {ws_sent}
xbridge_ws_frames_total {{direction=\"received\"}} {ws_recv}
# HELP xbridge_webhooks_total Total webhook deliveries
# TYPE xbridge_webhooks_total counter
xbridge_webhooks_total {{result=\"success\"}} {wh_ok}
xbridge_webhooks_total {{result=\"failure\"}} {wh_fail}
# HELP xbridge_trunk_calls_total Total calls from trunk host peers
# TYPE xbridge_trunk_calls_total counter
xbridge_trunk_calls_total {trunk_in}
# HELP xbridge_rate_limit_rejections_total HTTP requests rejected by rate limiter
# TYPE xbridge_rate_limit_rejections_total counter
xbridge_rate_limit_rejections_total {rate_limit}
",
            inbound = i.calls_inbound.load(Ordering::Relaxed),
            outbound = i.calls_outbound.load(Ordering::Relaxed),
            active = active_calls,
            http = i.http_requests.load(Ordering::Relaxed),
            ws = i.ws_connections.load(Ordering::Relaxed),
            ws_sent = i.ws_frames_sent.load(Ordering::Relaxed),
            ws_recv = i.ws_frames_received.load(Ordering::Relaxed),
            wh_ok = i.webhooks_sent.load(Ordering::Relaxed),
            wh_fail = i.webhooks_failed.load(Ordering::Relaxed),
            trunk_in = i.trunk_calls_inbound.load(Ordering::Relaxed),
            rate_limit = i.rate_limit_rejections.load(Ordering::Relaxed),
        );

        i.call_duration
            .render(&mut out, "xbridge_call_duration_seconds", "Call duration");
        i.http_request_duration.render(
            &mut out,
            "xbridge_http_request_duration_seconds",
            "HTTP request duration",
        );
        i.webhook_duration.render(
            &mut out,
            "xbridge_webhook_duration_seconds",
            "Webhook delivery duration",
        );

        out
    }
}

// ── Histogram ──────────────────────────────────────────────────────────────

/// Lock-free histogram with fixed buckets, compatible with Prometheus exposition format.
struct Histogram {
    /// Upper bounds for each bucket.
    bounds: &'static [f64],
    /// Atomic counter for each bucket (index-aligned with `bounds`).
    buckets: Vec<AtomicU64>,
    /// Total count of observations.
    count: AtomicU64,
    /// Sum of all observed values, stored as f64 bits.
    sum_bits: AtomicU64,
}

/// Default buckets suitable for latencies in seconds.
const DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Buckets for call duration (seconds).
const CALL_DURATION_BUCKETS: &[f64] = &[
    1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
];

impl Histogram {
    fn new(bounds: &'static [f64]) -> Self {
        let buckets = (0..bounds.len()).map(|_| AtomicU64::new(0)).collect();
        Self {
            bounds,
            buckets,
            count: AtomicU64::new(0),
            sum_bits: AtomicU64::new(0_f64.to_bits()),
        }
    }

    fn observe(&self, value: f64) {
        // Find the first bucket whose upper bound contains this value.
        for (i, &bound) in self.bounds.iter().enumerate() {
            if value <= bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        // Atomic f64 add via CAS loop
        loop {
            let current = self.sum_bits.load(Ordering::Relaxed);
            let new = f64::from_bits(current) + value;
            if self
                .sum_bits
                .compare_exchange_weak(current, new.to_bits(), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    fn render(&self, out: &mut String, name: &str, help: &str) {
        use std::fmt::Write;
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} histogram");
        let mut cumulative = 0u64;
        for (i, &bound) in self.bounds.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {cumulative}");
        }
        let total = self.count.load(Ordering::Relaxed);
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {total}");
        let sum = f64::from_bits(self.sum_bits.load(Ordering::Relaxed));
        let _ = writeln!(out, "{name}_sum {sum}");
        let _ = writeln!(out, "{name}_count {total}");
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new(DEFAULT_BUCKETS)
    }
}

impl Default for MetricsInner {
    fn default() -> Self {
        Self {
            calls_inbound: AtomicU64::default(),
            calls_outbound: AtomicU64::default(),
            webhooks_sent: AtomicU64::default(),
            webhooks_failed: AtomicU64::default(),
            http_requests: AtomicU64::default(),
            ws_connections: AtomicU64::default(),
            trunk_calls_inbound: AtomicU64::default(),
            rate_limit_rejections: AtomicU64::default(),
            ws_frames_sent: AtomicU64::default(),
            ws_frames_received: AtomicU64::default(),
            call_duration: Histogram::new(CALL_DURATION_BUCKETS),
            http_request_duration: Histogram::default(),
            webhook_duration: Histogram::default(),
        }
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

    #[test]
    fn histogram_buckets_and_render() {
        let m = Metrics::new();
        m.observe_call_duration(5.0);
        m.observe_call_duration(15.0);
        m.observe_call_duration(65.0);

        let output = m.render(0);
        // 5.0 falls into le=5 bucket
        assert!(output.contains("xbridge_call_duration_seconds_bucket{le=\"5\"} 1"));
        // 15.0 falls into le=30 bucket (cumulative: 2)
        assert!(output.contains("xbridge_call_duration_seconds_bucket{le=\"30\"} 2"));
        // All 3 in +Inf
        assert!(output.contains("xbridge_call_duration_seconds_bucket{le=\"+Inf\"} 3"));
        assert!(output.contains("xbridge_call_duration_seconds_sum 85"));
        assert!(output.contains("xbridge_call_duration_seconds_count 3"));
    }

    #[test]
    fn http_duration_histogram_renders() {
        let m = Metrics::new();
        m.observe_http_request_duration(0.002);
        m.observe_http_request_duration(0.05);

        let output = m.render(0);
        assert!(output.contains("xbridge_http_request_duration_seconds_bucket{le=\"0.005\"} 1"));
        assert!(output.contains("xbridge_http_request_duration_seconds_count 2"));
    }

    #[test]
    fn ws_frames_counter() {
        let m = Metrics::new();
        m.inc_ws_frames_sent();
        m.inc_ws_frames_sent();
        m.inc_ws_frames_received();

        let output = m.render(0);
        assert!(output.contains("xbridge_ws_frames_total {direction=\"sent\"} 2"));
        assert!(output.contains("xbridge_ws_frames_total {direction=\"received\"} 1"));
    }

    #[test]
    fn rate_limit_counter() {
        let m = Metrics::new();
        m.inc_rate_limit_rejections();
        m.inc_rate_limit_rejections();

        let output = m.render(0);
        assert!(output.contains("xbridge_rate_limit_rejections_total 2"));
    }
}
