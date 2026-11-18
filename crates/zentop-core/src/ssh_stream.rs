//! The bounded JSON Lines exchange used by the SSH Hosts dashboard.
use crate::{monitoring::SystemStats, sampling::Frame};
use serde::{Deserialize, Serialize};
use std::{io, time::Duration};

pub const VERSION: u32 = 1;
pub const INTERVAL: Duration = Duration::from_secs(3);
pub const MAX_SAMPLES: u32 = 1200;
pub const MAX_LINE_BYTES: usize = 32 * 1024;
pub const MAX_PROCESSES: usize = 24;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    pub cpu_percent: Option<f64>,
    pub memory_used: u64,
    pub memory_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    pub uptime_seconds: u64,
}

impl From<&SystemStats> for Metrics {
    fn from(stats: &SystemStats) -> Self {
        Self {
            cpu_percent: crate::sampling::average_cpu(&stats.cpu_usage),
            memory_used: stats.memory_used,
            memory_total: stats.memory_total,
            disk_used: stats.disk_used_bytes,
            disk_total: stats.disk_total_bytes,
            uptime_seconds: stats.uptime,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessReading {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub version: u32,
    pub sequence: u32,
    pub captured_at_ms: u64,
    pub metrics: Metrics,
    pub processes: Vec<ProcessReading>,
}

impl Sample {
    /// Projects the shared collector's snapshot into the small on-wire schema.
    pub fn from_frame(sequence: u32, frame: &Frame) -> Self {
        let mut busiest: Vec<_> = frame
            .processes
            .iter()
            .filter(|process| process.cpu_usage.is_finite() && process.cpu_usage >= 0.)
            .collect();
        let order = |a: &&crate::monitoring::ProcessInfo, b: &&crate::monitoring::ProcessInfo| {
            b.cpu_usage
                .total_cmp(&a.cpu_usage)
                .then_with(|| b.memory_usage.cmp(&a.memory_usage))
                .then_with(|| a.pid.cmp(&b.pid))
        };
        if busiest.len() > MAX_PROCESSES {
            busiest.select_nth_unstable_by(MAX_PROCESSES, order);
            busiest.truncate(MAX_PROCESSES);
        }
        busiest.sort_unstable_by(order);
        Self {
            version: VERSION,
            sequence,
            captured_at_ms: frame.captured_at,
            metrics: Metrics::from(&frame.stats),
            processes: busiest
                .into_iter()
                .map(|process| ProcessReading {
                    pid: process.pid,
                    name: safe_display(&process.name, 80),
                    cpu_percent: process.cpu_usage,
                    memory_bytes: process.memory_usage,
                })
                .collect(),
        }
    }
}

/// Remove controls that can change the way a remote string is displayed.
pub fn safe_display(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(c, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(limit)
        .collect()
}

/// Decode one complete line within the frame budget.
pub fn decode_sample(line: &[u8]) -> io::Result<Sample> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    if line.len() > MAX_LINE_BYTES {
        return Err(invalid("SSH reading exceeded 32 KiB."));
    }
    if line.last() != Some(&b'\n') {
        return Err(invalid("SSH stream ended during a reading."));
    }
    let mut sample: Sample = serde_json::from_slice(line).map_err(|_| {
        invalid("Invalid SSH reading. Check that the host runs zentop-cli stream v1.")
    })?;
    if sample.version != VERSION {
        return Err(invalid(
            "Unsupported SSH stream version. Install a compatible zentop-cli.",
        ));
    }
    if !(1..=MAX_SAMPLES).contains(&sample.sequence)
        || sample
            .metrics
            .cpu_percent
            .is_some_and(|value| !(0.0..=100.0).contains(&value))
        || sample.metrics.memory_used > sample.metrics.memory_total
        || sample.metrics.disk_used > sample.metrics.disk_total
        || sample.processes.len() > MAX_PROCESSES
        || sample
            .processes
            .iter()
            .any(|process| !process.cpu_percent.is_finite() || process.cpu_percent < 0.)
    {
        return Err(invalid("SSH reading contained invalid values."));
    }
    for process in &mut sample.processes {
        process.name = safe_display(&process.name, 80);
    }
    Ok(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_oversized_and_bad_version_frames_fail() {
        assert!(decode_sample(b"{}").is_err());
        let large = vec![b'x'; MAX_LINE_BYTES + 1];
        assert!(decode_sample(&large).is_err());
        let sample = br#"{"version":2,"sequence":1,"captured_at_ms":0,"metrics":{"cpu_percent":null,"memory_used":0,"memory_total":0,"disk_used":0,"disk_total":0,"uptime_seconds":0},"processes":[]}
"#;
        assert!(decode_sample(sample)
            .unwrap_err()
            .to_string()
            .contains("version"));
    }
}
