//! Domain-specific counters/gauges, recorded through the same global
//! `metrics` recorder `metrics_adapter::metrics_layer()` installs (see
//! `main.rs`) -- so these show up on the same `/metrics` endpoint as the
//! generic HTTP request metrics, with no second registry/route to run.

pub fn auth_attempt(result: &'static str) {
    metrics::counter!("idp_auth_attempts_total", "result" => result).increment(1);
}

pub fn token_issued(kind: &'static str) {
    metrics::counter!("idp_tokens_issued_total", "type" => kind).increment(1);
}

pub fn rate_limited(endpoint: &'static str) {
    metrics::counter!("idp_rate_limited_total", "endpoint" => endpoint).increment(1);
}

pub fn refresh_rotation() {
    metrics::counter!("idp_refresh_token_rotations_total").increment(1);
}

pub fn registered_users_gauge(count: f64) {
    metrics::gauge!("idp_registered_users_total").set(count);
}

pub fn active_sessions(delta: f64) {
    metrics::gauge!("idp_active_sessions").increment(delta);
}
