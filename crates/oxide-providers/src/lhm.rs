//! LibreHardwareMonitor HTTP provider.
//!
//! LHM 0.9.5+ runs a small HTTP server (default `http://127.0.0.1:8085/`) with a
//! Prometheus `/metrics` endpoint and a `/data.json` tree. `/metrics` is preferred:
//! invariant culture, base units, smaller payload.
//!
//! Security: the URL is user configurable, so only loopback hosts are accepted. Connecting
//! to anything else is refused even if the user asks for it — LHM's server has no
//! authentication by default and must never be reached across the network.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use thiserror::Error;
use ureq::http::Uri;

use crate::ProviderReadings;
use crate::heuristics::TemperaturePicker;

/// Whole-request budget for a single poll. `ExternalSensors::poll` is called once per tick.
const REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
/// Cheap TCP preflight budget: a missing listener is reported in ~this time instead of
/// waiting the full HTTP budget.
const PRECHECK_TIMEOUT: Duration = Duration::from_millis(120);
/// Do not start the `/data.json` fallback later than this, to respect the 500 ms budget.
const FALLBACK_BUDGET: Duration = Duration::from_millis(200);
/// After `/metrics` answers 404, retry it occasionally instead of every tick.
const METRICS_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Error)]
pub(crate) enum LhmError {
    #[error("invalid LHM URL: {0}")]
    InvalidUrl(String),
    #[error("refusing non-loopback LHM host '{0}'")]
    NotLoopback(String),
    #[error("LHM web server is not running at {0}")]
    NotRunning(String),
    #[error("LHM request failed: {0}")]
    Network(String),
    #[error("LHM returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("LHM response did not contain usable temperatures ({0})")]
    Malformed(&'static str),
    #[error("LHM did not respond within {0} ms")]
    Timeout(u64),
}

pub(crate) struct LhmClient {
    metrics_url: String,
    data_url: String,
    host: String,
    port: u16,
    /// `host:port` for user-facing “not running” details.
    endpoint: String,
    agent: ureq::Agent,
    /// Set after a 404 on `/metrics`; `/data.json` is used until the retry deadline.
    metrics_missing_until: Option<Instant>,
}

impl LhmClient {
    pub(crate) fn new(url: &str) -> Result<Self, LhmError> {
        let uri = Uri::from_str(url).map_err(|err| LhmError::InvalidUrl(err.to_string()))?;
        let host = uri
            .host()
            .ok_or_else(|| LhmError::InvalidUrl("missing host".to_string()))?;
        if !is_loopback_host(host) {
            return Err(LhmError::NotLoopback(host.to_string()));
        }
        if uri.scheme_str() != Some("http") {
            return Err(LhmError::InvalidUrl(
                "only plain http:// loopback URLs are supported".to_string(),
            ));
        }

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .new_agent();

        let port = uri.port_u16().unwrap_or(80);
        let endpoint = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };

        Ok(Self {
            metrics_url: url.to_string(),
            data_url: sibling_url(url, "data.json"),
            host: host.to_string(),
            port,
            endpoint,
            agent,
            metrics_missing_until: None,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<ProviderReadings, LhmError> {
        let started = Instant::now();
        // Distinguish "nothing listening" (fast, actionable) from "accepted but hung"
        // (Timeout/Stale) before spending the full HTTP budget.
        self.preflight()?;

        let skip_metrics = self
            .metrics_missing_until
            .is_some_and(|deadline| started < deadline);

        if !skip_metrics {
            let budget = REQUEST_TIMEOUT.saturating_sub(started.elapsed());
            if budget.is_zero() {
                return Err(LhmError::Timeout(REQUEST_TIMEOUT.as_millis() as u64));
            }
            match self.fetch(&self.metrics_url, budget) {
                Ok((200, body)) => {
                    let readings = parse_prometheus(&body);
                    if readings.is_empty() {
                        return Err(LhmError::Malformed("no temperature metrics"));
                    }
                    return Ok(readings);
                }
                Ok((404, _)) => {
                    // Older/newer LHM builds may not expose /metrics; fall back to JSON.
                    self.metrics_missing_until = Some(started + METRICS_RETRY);
                }
                Ok((status, _)) => return Err(LhmError::HttpStatus(status)),
                Err(err) => return Err(err),
            }
        }

        if started.elapsed() > FALLBACK_BUDGET {
            return Err(LhmError::Timeout(REQUEST_TIMEOUT.as_millis() as u64));
        }
        // Keep the whole poll within REQUEST_TIMEOUT even when both endpoints are tried.
        let remaining = REQUEST_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(LhmError::Timeout(REQUEST_TIMEOUT.as_millis() as u64));
        }

        let (status, body) = self.fetch(&self.data_url, remaining)?;
        if status != 200 {
            return Err(LhmError::HttpStatus(status));
        }
        let readings = parse_data_json(&body);
        if readings.is_empty() {
            return Err(LhmError::Malformed("no temperatures in data.json"));
        }
        Ok(readings)
    }

    fn fetch(&self, url: &str, timeout: Duration) -> Result<(u16, String), LhmError> {
        let mut response = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
            .map_err(|err| self.classify(err))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|err| self.classify(err))?;
        Ok((status, body))
    }

    /// Classify transport errors so the UI can distinguish “not running” from “hung”.
    fn classify(&self, error: ureq::Error) -> LhmError {
        match error {
            ureq::Error::Timeout(_) => LhmError::Timeout(REQUEST_TIMEOUT.as_millis() as u64),
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
                LhmError::NotRunning(self.endpoint.clone())
            }
            ureq::Error::Io(error) => {
                if is_unreachable_io(&error) {
                    LhmError::NotRunning(self.endpoint.clone())
                } else {
                    LhmError::Network(error.to_string())
                }
            }
            other => LhmError::Network(other.to_string()),
        }
    }

    /// Cheap loopback TCP preflight (bounded to ~[`PRECHECK_TIMEOUT`]).
    ///
    /// A refused/reset/timed-out connect means there is no LHM web server; return
    /// immediately instead of waiting for the HTTP budget.
    fn preflight(&self) -> Result<(), LhmError> {
        let deadline = Instant::now() + PRECHECK_TIMEOUT;
        let Ok(addresses) = (self.host.as_str(), self.port).to_socket_addrs() else {
            return Err(LhmError::NotRunning(self.endpoint.clone()));
        };
        for address in addresses {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if TcpStream::connect_timeout(&address, remaining).is_ok() {
                return Ok(());
            }
        }
        Err(LhmError::NotRunning(self.endpoint.clone()))
    }
}

/// Windows/Linux socket errors that mean “nothing is listening / unreachable”.
fn is_unreachable_io(error: &io::Error) -> bool {
    use io::ErrorKind::{
        ConnectionAborted, ConnectionRefused, ConnectionReset, NotFound, TimedOut,
    };
    matches!(
        error.kind(),
        ConnectionRefused | ConnectionReset | ConnectionAborted | NotFound | TimedOut
    ) || matches!(
        error.raw_os_error(),
        // WSAECONNREFUSED, WSAETIMEDOUT, WSAENETUNREACH, WSAEHOSTUNREACH, WSAHOST_NOT_FOUND
        Some(10051 | 10060 | 10061 | 10065 | 11001)
    )
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// `http://host:port/metrics` → `http://host:port/data.json`.
fn sibling_url(url: &str, file: &str) -> String {
    let scheme_end = url.find("://").map(|index| index + 3).unwrap_or(0);
    match url.rfind('/') {
        Some(index) if index >= scheme_end => format!("{}/{}", &url[..index], file),
        _ => format!("{url}/{file}"),
    }
}

// ---------------------------------------------------------------------------------------
// Prometheus text format
// ---------------------------------------------------------------------------------------

/// Parse LHM's Prometheus exposition format.
///
/// Lines look like:
/// `lhm_cpu_temperature_celsius{sensorName="Core (Tctl/Tdie)",sensorId="/amdcpu/0/temperature/0",hardwareId="/amdcpu/0"} 51.5`
pub(crate) fn parse_prometheus(text: &str) -> ProviderReadings {
    let mut picker = TemperaturePicker::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, labels, rest)) = split_sample(line) else {
            continue;
        };
        if !is_temperature_metric(name) {
            continue;
        }
        let Some(value) = parse_sample_value(rest) else {
            continue;
        };

        let sensor_name = label_value(labels, "sensorName").unwrap_or_default();
        let sensor_id = label_value(labels, "sensorId").unwrap_or_default();
        let hardware_id = label_value(labels, "hardwareId").unwrap_or_default();

        // LHM hardware ids look like `/amdcpu/0`, `/nvme/0`, so include the sensor id:
        // it disambiguates sensors whose parent name is missing.
        let device = format!("{hardware_id} {sensor_id}");
        picker.offer(&device, &sensor_name, value as f32);
    }

    picker.finish()
}

fn split_sample(line: &str) -> Option<(&str, &str, &str)> {
    let name_end = line.find(|ch: char| ch == '{' || ch.is_whitespace())?;
    let name = &line[..name_end];
    if name.is_empty() {
        return None;
    }

    let mut rest = &line[name_end..];
    let labels = if let Some(after_brace) = rest.strip_prefix('{') {
        let end = after_brace.find('}')?;
        let labels = &after_brace[..end];
        rest = &after_brace[end + 1..];
        labels
    } else {
        ""
    };
    Some((name, labels, rest))
}

fn is_temperature_metric(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if !name.contains("temperature") {
        return false;
    }
    name.contains("celsius")
        || name.ends_with("_c")
        || name.starts_with("lhm_")
        || name.starts_with("librehardwaremonitor_")
}

fn parse_sample_value(rest: &str) -> Option<f64> {
    // `value [timestamp]`; ignore the optional timestamp.
    let token = rest.split_whitespace().next()?;
    let token = token.trim_end_matches(',');
    token.parse::<f64>().ok()
}

/// Extract `key="value"` from a label block, honouring escaped quotes.
fn label_value(labels: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let mut search_from = 0;

    while let Some(relative) = labels[search_from..].find(&needle) {
        let at = search_from + relative;
        let boundary_ok = at == 0
            || matches!(
                labels.as_bytes()[at - 1],
                b',' | b'{' | b' ' | b'\t' | b'\n'
            );
        let start = at + needle.len();
        if boundary_ok && let Some(len) = find_closing_quote(&labels[start..]) {
            return Some(unescape_label(&labels[start..start + len]));
        }
        search_from = start;
    }
    None
}

fn find_closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(index),
            _ => {}
        }
    }
    None
}

fn unescape_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// /data.json fallback
// ---------------------------------------------------------------------------------------

/// Parse the LHM `/data.json` tree. `Value` is a locally formatted string such as
/// `"51,5 °C"`, so only the numeric prefix is used.
pub(crate) fn parse_data_json(text: &str) -> ProviderReadings {
    let mut picker = TemperaturePicker::new();
    if let Ok(root) = serde_json::from_str::<Json>(text) {
        walk_json(&root, &mut Vec::new(), &mut picker);
    }
    picker.finish()
}

fn walk_json(node: &Json, parents: &mut Vec<String>, picker: &mut TemperaturePicker) {
    let text = node.get("Text").and_then(Json::as_str).unwrap_or("");
    let sensor_id = node.get("SensorId").and_then(Json::as_str).unwrap_or("");
    let node_type = node.get("Type").and_then(Json::as_str).unwrap_or("");

    if node_type.eq_ignore_ascii_case("temperature")
        && let Some(value) = node.get("Value").and_then(json_number)
    {
        let device = format!("{} {}", parents.join(" "), sensor_id);
        picker.offer(&device, text, value as f32);
    }

    if let Some(children) = node.get("Children").and_then(Json::as_array) {
        if !text.is_empty() {
            parents.push(text.to_string());
        }
        for child in children {
            walk_json(child, parents, picker);
        }
        if !text.is_empty() {
            parents.pop();
        }
    }
}

fn json_number(value: &Json) -> Option<f64> {
    match value {
        Json::Number(number) => number.as_f64(),
        Json::String(text) => parse_formatted_number(text),
        _ => None,
    }
}

fn parse_formatted_number(text: &str) -> Option<f64> {
    let mut cleaned = String::new();
    for ch in text.trim().chars() {
        match ch {
            '0'..='9' | '+' | '-' => cleaned.push(ch),
            '.' => {
                if cleaned.contains('.') {
                    break;
                }
                cleaned.push(ch);
            }
            ',' => {
                if cleaned.contains('.') {
                    break;
                }
                cleaned.push('.');
            }
            _ => break,
        }
    }
    cleaned.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const METRICS: &str = r#"# HELP lhm_cpu_temperature_celsius CPU temperature
# TYPE lhm_cpu_temperature_celsius gauge
lhm_cpu_temperature_celsius{sensorName="CPU Package",sensorId="/intelcpu/0/temperature/0",hardwareId="/intelcpu/0"} 55.5 1700000000000
lhm_gpu_temperature_celsius{sensorName="GPU Core",sensorId="/gpu-nvidia/0/temperature/0",hardwareId="/gpu-nvidia/0"} 47
lhm_disk_temperature_celsius{sensorName="Temperature",sensorId="/nvme/0/temperature/0",hardwareId="/nvme/0"} 41.25
lhm_motherboard_temperature_celsius{sensorName="System",sensorId="/motherboard/0/temperature/0",hardwareId="/motherboard/0"} 35
lhm_cpu_temperature_celsius{sensorName="CPU Core #2",sensorId="/intelcpu/0/temperature/2",hardwareId="/intelcpu/0"} NaN
lhm_cpu_temperature_celsius{sensorName="CPU Core #3",sensorId="/intelcpu/0/temperature/3",hardwareId="/intelcpu/0"} 300
lhm_fan_speed_rpm{sensorName="CPU Fan",sensorId="/intelcpu/0/fan/0",hardwareId="/intelcpu/0"} 1200
"#;

    #[test]
    fn parses_prometheus_metrics() {
        let readings = parse_prometheus(METRICS);
        assert_eq!(readings.cpu_temp_c, Some(55.5));
        assert_eq!(readings.gpu_temp_c, Some(47.0));
        assert_eq!(readings.disk_temp_c, Some(41.25));
        assert_eq!(readings.mainboard_temp_c, Some(35.0));
    }

    #[test]
    fn handles_commas_inside_labels_and_escapes() {
        let text = r#"lhm_cpu_temperature_celsius{sensorName="Core (Tctl, Tdie)",sensorId="/amdcpu/0/temperature/0",hardwareId="/amdcpu/0"} 51.5"#;
        assert_eq!(parse_prometheus(text).cpu_temp_c, Some(51.5));

        let escaped = r#"lhm_cpu_temperature_celsius{sensorName="CPU \"Package\"",sensorId="/amdcpu/0/temperature/1",hardwareId="/amdcpu/0"} 61.0"#;
        assert_eq!(parse_prometheus(escaped).cpu_temp_c, Some(61.0));
    }

    #[test]
    fn skips_bad_lines_without_panicking() {
        let text = "\n# comment\nbroken line\nlhm_cpu_temperature_celsius 51.5\n\
                    lhm_cpu_temperature_celsius{} not-a-number\n\
                    lhm_cpu_temperature_celsius{sensorName=\"x\"} inf\n";
        assert!(parse_prometheus(text).is_empty());
    }

    #[test]
    fn parses_value_with_comma_and_suffix() {
        assert_eq!(parse_formatted_number("51,5 °C"), Some(51.5));
        assert_eq!(parse_formatted_number("47.0 °C"), Some(47.0));
        assert_eq!(parse_formatted_number("-5.5"), Some(-5.5));
        assert_eq!(parse_formatted_number("n/a"), None);
    }

    #[test]
    fn parses_data_json_tree() {
        let json = r#"{
            "Text": "AMD Ryzen 7 7800X3D",
            "SensorId": "/amdcpu/0",
            "Type": "Cpu",
            "Value": "",
            "Children": [
                {"Text":"Core (Tctl/Tdie)","SensorId":"/amdcpu/0/temperature/0","Type":"Temperature","Value":"50,5 °C","Children":[]},
                {"Text":"CPU Package","SensorId":"/amdcpu/0/temperature/1","Type":"Temperature","Value":"55.0 °C","Children":[]}
            ]
        }"#;
        assert_eq!(parse_data_json(json).cpu_temp_c, Some(55.0));

        let tree = r#"{
            "Text": "PC",
            "SensorId": "/",
            "Type": "Computer",
            "Children": [
                {"Text":"NVIDIA GeForce GTX 1650","SensorId":"/gpu-nvidia/0","Type":"Gpu","Children":[
                    {"Text":"GPU Core","SensorId":"/gpu-nvidia/0/temperature/0","Type":"Temperature","Value":"47.0 °C","Children":[]}
                ]},
                {"Text":"Samsung SSD 990 PRO","SensorId":"/nvme/0","Type":"Storage","Children":[
                    {"Text":"Temperature","SensorId":"/nvme/0/temperature/0","Type":"Temperature","Value":"41,0 °C","Children":[]}
                ]},
                {"Text":"ASUS ROG STRIX B650E-F","SensorId":"/motherboard/0","Type":"Motherboard","Children":[
                    {"Text":"System","SensorId":"/motherboard/0/temperature/0","Type":"Temperature","Value":"35.0 °C","Children":[]}
                ]}
            ]
        }"#;
        let readings = parse_data_json(tree);
        assert_eq!(readings.gpu_temp_c, Some(47.0));
        assert_eq!(readings.disk_temp_c, Some(41.0));
        assert_eq!(readings.mainboard_temp_c, Some(35.0));
    }

    #[test]
    fn rejects_non_loopback_urls() {
        assert!(LhmClient::new("http://127.0.0.1:8085/metrics").is_ok());
        assert!(LhmClient::new("http://localhost:8085/metrics").is_ok());
        assert!(LhmClient::new("http://LocalHost:8085/metrics").is_ok());

        assert!(matches!(
            LhmClient::new("http://192.168.1.10:8085/metrics"),
            Err(LhmError::NotLoopback(_))
        ));
        assert!(matches!(
            LhmClient::new("http://example.com/metrics"),
            Err(LhmError::NotLoopback(_))
        ));
        assert!(matches!(
            LhmClient::new("https://127.0.0.1:8085/metrics"),
            Err(LhmError::InvalidUrl(_))
        ));
        assert!(matches!(
            LhmClient::new("not a url"),
            Err(LhmError::InvalidUrl(_))
        ));
    }

    #[test]
    fn closed_loopback_port_reports_not_running_without_waiting_the_http_budget() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let mut client = LhmClient::new(&format!("http://{addr}/metrics")).unwrap();
        let started = Instant::now();
        let error = client.poll().expect_err("closed port must not succeed");
        let elapsed = started.elapsed();

        match error {
            LhmError::NotRunning(endpoint) => assert_eq!(endpoint, addr.to_string()),
            other => panic!("expected NotRunning, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_millis(300),
            "preflight should fail fast, took {elapsed:?}"
        );
    }

    #[test]
    fn accepted_but_silent_server_reports_timeout() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let held = thread::spawn(move || {
            listener.set_nonblocking(true).ok();
            let deadline = Instant::now() + Duration::from_millis(900);
            let mut streams = Vec::new();
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => streams.push(stream),
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        let mut client = LhmClient::new(&format!("http://{addr}/metrics")).unwrap();
        let started = Instant::now();
        let error = client.poll().expect_err("silent server must time out");
        let elapsed = started.elapsed();
        let _ = held.join();

        assert!(
            matches!(error, LhmError::Timeout(_)),
            "expected Timeout, got {error:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(300),
            "timeout returned too fast: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "timeout waited too long: {elapsed:?}"
        );
    }

    #[test]
    fn connection_errors_are_classified_as_not_running() {
        let client = LhmClient::new("http://127.0.0.1:8085/metrics").unwrap();
        let error = client.classify(ureq::Error::Io(io::Error::from_raw_os_error(10061)));
        assert!(
            matches!(error, LhmError::NotRunning(ref endpoint) if endpoint == "127.0.0.1:8085"),
            "{error:?}"
        );

        assert!(is_unreachable_io(&io::Error::from_raw_os_error(10061)));
        assert!(is_unreachable_io(&io::Error::from_raw_os_error(10065)));
        assert!(is_unreachable_io(&io::Error::from_raw_os_error(11001)));
        assert!(!is_unreachable_io(&io::Error::other("unexpected")));
    }
    #[test]
    fn builds_sibling_url() {
        assert_eq!(
            sibling_url("http://127.0.0.1:8085/metrics", "data.json"),
            "http://127.0.0.1:8085/data.json"
        );
        assert_eq!(
            sibling_url("http://127.0.0.1:8085", "data.json"),
            "http://127.0.0.1:8085/data.json"
        );
    }

    #[test]
    fn picks_best_cpu_temperature() {
        let text = "\
            lhm_cpu_temperature_celsius{sensorName=\"CPU Core Max\",sensorId=\"/intelcpu/0/temperature/9\",hardwareId=\"/intelcpu/0\"} 70\n\
            lhm_cpu_temperature_celsius{sensorName=\"CPU Package\",sensorId=\"/intelcpu/0/temperature/0\",hardwareId=\"/intelcpu/0\"} 60\n";
        assert_eq!(parse_prometheus(text).cpu_temp_c, Some(60.0));
    }
}
