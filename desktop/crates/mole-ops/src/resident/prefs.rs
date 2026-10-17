// Menu-bar preferences: which metrics the status item shows and in what
// order, compact spacing, the logo, and per-metric color rules. Persisted as
// JSON at ~/.config/mole/resident.json; a missing or unreadable file yields
// the defaults (CPU + memory, logo on, colors on).

use super::MetricId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Every metric in its default display order.
pub const DEFAULT_ORDER: [MetricId; 13] = [
    MetricId::Cpu,
    MetricId::CpuTemperature,
    MetricId::Gpu,
    MetricId::GpuTemperature,
    MetricId::Memory,
    MetricId::Disk,
    MetricId::DiskTemperature,
    MetricId::BatteryTemperature,
    MetricId::ChargingPower,
    MetricId::BatteryPower,
    MetricId::SystemPower,
    MetricId::FanSpeed,
    MetricId::Network,
];

/// Status-item color for a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Normal,
    Green,
    Yellow,
    Blue,
    Warning,
    Critical,
}

/// Rule condition: `below` (< a), `range` (a ≤ v < b), `at_least` (≥ a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Below,
    Range,
    AtLeast,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorRule {
    pub condition: Condition,
    pub a: f32,
    /// Upper bound, used by `range` only.
    pub b: Option<f32>,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricEntry {
    pub id: MetricId,
    pub enabled: bool,
    /// Rules checked in order; first match wins; none = Normal.
    #[serde(default)]
    pub rules: Vec<ColorRule>,
}

/// Fan column mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanDisplay {
    #[default]
    Highest,
    Average,
    Selected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResidentPrefs {
    /// Master switch: "Show in menu bar".
    pub enabled: bool,
    pub show_icon: bool,
    pub compact: bool,
    pub usage_colors: bool,
    pub metrics: Vec<MetricEntry>,
    #[serde(default)]
    pub fan_display: FanDisplay,
    /// Fan ids for `FanDisplay::Selected` (max 2).
    #[serde(default)]
    pub fan_ids: Vec<u32>,
    /// None = automatic (default route interface).
    #[serde(default)]
    pub network_interface: Option<String>,
}

/// Default rules for a metric (usage 70/90 %, temperatures, fans).
pub fn default_rules(id: MetricId) -> Vec<ColorRule> {
    let three = |lo: f32, hi: f32| {
        vec![
            ColorRule {
                condition: Condition::Below,
                a: lo,
                b: None,
                tone: Tone::Green,
            },
            ColorRule {
                condition: Condition::Range,
                a: lo,
                b: Some(hi),
                tone: Tone::Warning,
            },
            ColorRule {
                condition: Condition::AtLeast,
                a: hi,
                b: None,
                tone: Tone::Critical,
            },
        ]
    };
    match id {
        MetricId::Cpu | MetricId::Gpu | MetricId::Memory | MetricId::Disk => three(70.0, 90.0),
        MetricId::CpuTemperature | MetricId::GpuTemperature => three(60.0, 80.0),
        MetricId::DiskTemperature => three(50.0, 60.0),
        MetricId::BatteryTemperature => three(40.0, 45.0),
        MetricId::FanSpeed => three(2000.0, 4000.0),
        MetricId::ChargingPower
        | MetricId::BatteryPower
        | MetricId::SystemPower
        | MetricId::Network => Vec::new(),
    }
}

impl Default for ResidentPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            show_icon: true,
            compact: false,
            usage_colors: true,
            metrics: DEFAULT_ORDER
                .iter()
                .map(|id| MetricEntry {
                    id: *id,
                    enabled: matches!(id, MetricId::Cpu | MetricId::Memory),
                    rules: default_rules(*id),
                })
                .collect(),
            fan_display: FanDisplay::Highest,
            fan_ids: vec![0],
            network_interface: None,
        }
    }
}

impl ResidentPrefs {
    /// Repair a loaded value: drop duplicate/unknown entries, append metrics
    /// missing from an older file (disabled), cap fan ids at two.
    pub fn normalized(mut self) -> Self {
        let mut seen = std::collections::HashSet::new();
        self.metrics.retain(|m| seen.insert(m.id));
        for id in DEFAULT_ORDER {
            if !seen.contains(&id) {
                self.metrics.push(MetricEntry {
                    id,
                    enabled: false,
                    rules: default_rules(id),
                });
            }
        }
        self.fan_ids.truncate(2);
        self
    }

    /// Enabled metrics in display order.
    pub fn enabled_metrics(&self) -> Vec<MetricId> {
        self.metrics
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.id)
            .collect()
    }

    /// The logo shows when asked for, or when no metric would render.
    pub fn effective_icon(&self) -> bool {
        self.show_icon || self.metrics.iter().all(|m| !m.enabled)
    }

    /// Color for a displayed (rounded) value of `id`.
    pub fn tone_for(&self, id: MetricId, value: f32) -> Tone {
        if !self.usage_colors {
            return Tone::Normal;
        }
        let Some(entry) = self.metrics.iter().find(|m| m.id == id) else {
            return Tone::Normal;
        };
        let v = value.round();
        for rule in &entry.rules {
            let hit = match rule.condition {
                Condition::Below => v < rule.a,
                Condition::Range => v >= rule.a && rule.b.is_some_and(|b| v < b),
                Condition::AtLeast => v >= rule.a,
            };
            if hit {
                return rule.tone;
            }
        }
        Tone::Normal
    }
}

/// ~/.config/mole/resident.json
pub fn prefs_file(home: &str) -> PathBuf {
    PathBuf::from(home).join(".config/mole/resident.json")
}

/// Load prefs; defaults when the file is missing or malformed.
pub fn load(home: &str) -> ResidentPrefs {
    std::fs::read_to_string(prefs_file(home))
        .ok()
        .and_then(|text| serde_json::from_str::<ResidentPrefs>(&text).ok())
        .map(ResidentPrefs::normalized)
        .unwrap_or_default()
}

/// Persist prefs atomically (temp file + rename).
pub fn save(home: &str, prefs: &ResidentPrefs) -> std::io::Result<()> {
    let path = prefs_file(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(prefs).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_cpu_and_memory_with_icon() {
        let p = ResidentPrefs::default();
        assert_eq!(p.enabled_metrics(), vec![MetricId::Cpu, MetricId::Memory]);
        assert!(p.effective_icon());
        assert_eq!(p.metrics.len(), DEFAULT_ORDER.len());
    }

    #[test]
    fn tone_rules_use_rounded_value_and_first_match() {
        let p = ResidentPrefs::default();
        assert_eq!(p.tone_for(MetricId::Cpu, 69.4), Tone::Green);
        assert_eq!(p.tone_for(MetricId::Cpu, 69.6), Tone::Warning);
        assert_eq!(p.tone_for(MetricId::Cpu, 90.0), Tone::Critical);
        assert_eq!(p.tone_for(MetricId::Network, 5.0), Tone::Normal);
        let off = ResidentPrefs {
            usage_colors: false,
            ..ResidentPrefs::default()
        };
        assert_eq!(off.tone_for(MetricId::Cpu, 99.0), Tone::Normal);
    }

    #[test]
    fn normalize_dedups_and_appends_missing() {
        let mut p = ResidentPrefs::default();
        p.metrics.truncate(2);
        p.metrics.push(p.metrics[0].clone());
        p.fan_ids = vec![0, 1, 2];
        let n = p.normalized();
        assert_eq!(n.metrics.len(), DEFAULT_ORDER.len());
        assert_eq!(n.fan_ids, vec![0, 1]);
    }

    #[test]
    fn roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        assert_eq!(load(home), ResidentPrefs::default());
        let p = ResidentPrefs {
            compact: true,
            ..ResidentPrefs::default()
        };
        save(home, &p).unwrap();
        assert_eq!(load(home), p);
    }
}
