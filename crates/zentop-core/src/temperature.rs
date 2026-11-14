//! Read-only hardware temperatures; unknown values are never represented as zero.
use crate::gpu::GpuDevice;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct Sensor {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub source: String,
    pub celsius: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub critical: Option<f64>,
}
pub fn valid(value: f64) -> Option<f64> {
    (value.is_finite() && (-40.0..=150.0).contains(&value)).then_some(value)
}
pub fn hottest(sensors: &[Sensor]) -> Option<&Sensor> {
    sensors
        .iter()
        .filter(|s| s.celsius.and_then(valid).is_some())
        .max_by(|a, b| {
            a.celsius
                .unwrap_or_default()
                .total_cmp(&b.celsius.unwrap_or_default())
                .then_with(|| b.id.cmp(&a.id))
        })
}
pub fn format(value: Option<f64>) -> String {
    value
        .and_then(valid)
        .map_or_else(|| "Unavailable".into(), |v| format!("{v:.1}°C"))
}
#[derive(Debug, Default)]
pub struct Monitor {
    ranges: BTreeMap<String, (f64, f64)>,
}
thread_local! {
    // macOS HID sensor handles are not Send; keep ownership on the collection thread.
    static COMPONENTS: std::cell::RefCell<sysinfo::Components> = Default::default();
}
impl Monitor {
    pub fn collect(&mut self, gpus: &[GpuDevice]) -> Vec<Sensor> {
        let mut sensors: Vec<_> = COMPONENTS.with_borrow_mut(|components| {
            components.refresh(true);
            components
                .iter()
                .take(120)
                .map(|c| {
                    let label = c.label();
                    let lower = label.to_lowercase();
                    let kind = if lower.contains("cpu")
                        || lower.contains("core")
                        || lower.contains("package")
                    {
                        "CPU"
                    } else if lower.contains("gpu") {
                        "GPU"
                    } else if lower.contains("nvme")
                        || lower.contains("ssd")
                        || lower.contains("disk")
                        || lower.contains("drive")
                        || lower.contains("nand")
                    {
                        "Storage"
                    } else {
                        "Other"
                    };
                    Sensor {
                        id: format!("system:{label}"),
                        name: label.into(),
                        kind: kind.into(),
                        source: "System hardware sensor".into(),
                        celsius: c.temperature().and_then(|v| valid(v as f64)),
                        minimum: None,
                        maximum: None,
                        critical: c.critical().and_then(|v| valid(v as f64)),
                    }
                })
                .collect::<Vec<_>>()
        });
        sensors.extend(gpus.iter().take(8).map(|gpu| Sensor {
            id: format!("gpu:{}", gpu.id),
            name: gpu.name.clone(),
            kind: "GPU".into(),
            source: gpu.source.clone(),
            celsius: gpu.temperature.and_then(valid),
            minimum: None,
            maximum: None,
            critical: None,
        }));
        self.finish(sensors)
    }
    fn finish(&mut self, mut sensors: Vec<Sensor>) -> Vec<Sensor> {
        for sensor in &mut sensors {
            sensor.celsius = sensor.celsius.and_then(valid);
            sensor.critical = sensor.critical.and_then(valid);
        }
        // Duplicate labels represent one named source; prefer its highest valid reading.
        sensors.sort_by(|a, b| {
            a.id.cmp(&b.id).then_with(|| {
                b.celsius
                    .partial_cmp(&a.celsius)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        sensors.dedup_by(|a, b| a.id == b.id);
        sensors.truncate(128);
        self.ranges
            .retain(|id, _| sensors.iter().any(|s| &s.id == id));
        for sensor in &mut sensors {
            sensor.celsius = sensor.celsius.and_then(valid);
            if let Some(value) = sensor.celsius {
                let range = self
                    .ranges
                    .entry(sensor.id.clone())
                    .or_insert((value, value));
                range.0 = range.0.min(value);
                range.1 = range.1.max(value);
            }
            if let Some((min, max)) = self.ranges.get(&sensor.id) {
                sensor.minimum = Some(*min);
                sensor.maximum = Some(*max);
            }
        }
        for kind in ["CPU", "GPU", "Storage"] {
            if !sensors.iter().any(|s| s.kind == kind) {
                sensors.push(Sensor {
                    id: format!("unavailable:{kind}"),
                    name: format!("{kind} temperature"),
                    kind: kind.into(),
                    source: "No readable sensor reported".into(),
                    celsius: None,
                    minimum: None,
                    maximum: None,
                    critical: None,
                });
            }
        }
        sensors
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sensor(id: &str, value: Option<f64>) -> Sensor {
        Sensor {
            id: id.into(),
            name: id.into(),
            kind: "CPU".into(),
            source: "fixture".into(),
            celsius: value,
            minimum: None,
            maximum: None,
            critical: None,
        }
    }
    #[test]
    fn missing_and_invalid_are_distinct_from_real_zero_and_ranges_survive_missing_readings() {
        let mut monitor = Monitor::default();
        for value in [f64::NAN, f64::INFINITY, -100., 200.] {
            assert_eq!(valid(value), None);
        }
        assert_eq!(valid(0.), Some(0.));
        monitor.finish(vec![sensor("cpu", Some(50.))]);
        monitor.finish(vec![sensor("cpu", Some(70.))]);
        let missing = monitor.finish(vec![sensor("cpu", None)]);
        assert_eq!(
            (missing[0].celsius, missing[0].minimum, missing[0].maximum),
            (None, Some(50.), Some(70.))
        );
        assert!(hottest(&missing).is_none());
        let recovered = monitor.finish(vec![sensor("cpu", Some(60.))]);
        assert_eq!(format(hottest(&recovered).unwrap().celsius), "60.0°C");
    }
    #[test]
    fn sensor_and_range_storage_remain_bounded_and_duplicates_are_deterministic() {
        let mut monitor = Monitor::default();
        let data = monitor.finish(vec![sensor("cpu", Some(10.)), sensor("cpu", Some(80.))]);
        assert_eq!(hottest(&data).unwrap().celsius, Some(80.));
        for run in 0..3 {
            let sensors = monitor.finish(
                (0..500)
                    .map(|i| sensor(&format!("{run}-{i}"), Some(40.)))
                    .collect(),
            );
            assert!(sensors.len() <= 131);
            assert!(monitor.ranges.len() <= 128);
        }
    }
}
