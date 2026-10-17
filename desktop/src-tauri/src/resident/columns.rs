// Status-item layout: turns one reading plus the menu-bar prefs into
// fixed-width text columns. Pure data (no AppKit) so widths, units, tones and
// hysteresis are unit-tested here; `status_item` only paints what this builds.

use mole_ops::resident::prefs::{FanDisplay, ResidentPrefs, Tone};
use mole_ops::resident::{MetricId, ResidentReading};
use std::collections::HashMap;

/// Status-item image height in points (the menu bar's content height).
pub const HEIGHT: f64 = 22.0;
/// Logo slot edge in points, vertically centered in `HEIGHT`.
pub const LOGO: u32 = 18;
/// Placeholder for a value the sampler could not read.
const MISSING: &str = "—";

/// Horizontal gap between the logo and columns.
pub fn gap(compact: bool) -> u32 {
    if compact {
        2
    } else {
        6
    }
}

/// Fixed geometry of the network column: digits grow leftward inside a
/// reserved field and the unit keeps its own origin, so a changing rate
/// never shifts neighbouring text.
pub struct RateLayout {
    pub value_offset: u32,
    pub value_width: u32,
    pub unit_offset: u32,
    pub unit_width: u32,
}

/// Network column geometry for normal and compact spacing.
pub fn rate_layout(compact: bool) -> RateLayout {
    if compact {
        RateLayout {
            value_offset: 10,
            value_width: 21,
            unit_offset: 32,
            unit_width: 10,
        }
    } else {
        RateLayout {
            value_offset: 11,
            value_width: 24,
            unit_offset: 38,
            unit_width: 32,
        }
    }
}

/// Direction marker drawn next to a heading or rate: up = red (upload,
/// battery discharging), down = blue (download, battery charging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Up,
    Down,
}

/// What one column paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Small heading over a large toned value ("CPU" / "37%").
    Stacked {
        heading: String,
        arrow: Option<Arrow>,
        value: String,
        tone: Tone,
    },
    /// Two equal toned rows (two selected fans).
    Rows([(String, Tone); 2]),
    /// ↑ upload over ↓ download: (digits, unit) per row.
    Rates {
        up: (String, String),
        down: (String, String),
    },
}

/// One fixed-width column of the status item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub id: MetricId,
    pub width: u32,
    pub body: Body,
}

/// Total image width in points for the given columns and logo.
pub fn width(columns: &[Column], icon: bool, compact: bool) -> u32 {
    let items = columns.len() + usize::from(icon);
    let content: u32 = columns.iter().map(|c| c.width).sum::<u32>() + if icon { LOGO } else { 0 };
    content + gap(compact) * items.saturating_sub(1) as u32
}

/// Last tone per colored slot, for descending-boundary hysteresis. Keyed by
/// metric and row (fan pairs color each row separately).
#[derive(Default)]
pub struct ToneMemory(HashMap<(MetricId, u8), Tone>);

impl ToneMemory {
    /// Tone for a displayed value, holding the previous tone while the value
    /// sits just below the boundary it crossed downward. Without this a CPU
    /// hovering at 69–70 % would flicker between two colors every second.
    fn tone(&mut self, prefs: &ResidentPrefs, id: MetricId, row: u8, value: f32) -> Tone {
        let candidate = prefs.tone_for(id, value);
        let margin = hysteresis_margin(id);
        let key = (id, row);
        if let Some(previous) = self.0.get(&key).copied() {
            // Rising never holds: tone_for(value + margin) only equals the
            // previous tone when the value fell less than `margin` below it.
            if previous != candidate && prefs.tone_for(id, value + margin) == previous {
                return previous;
            }
        }
        self.0.insert(key, candidate);
        candidate
    }

    /// Forget a slot whose value became unavailable.
    fn clear(&mut self, id: MetricId, row: u8) {
        self.0.remove(&(id, row));
    }
}

/// Hysteresis width per metric unit: 2 °C, 100 RPM, otherwise 3 points.
fn hysteresis_margin(id: MetricId) -> f32 {
    match id {
        MetricId::CpuTemperature
        | MetricId::GpuTemperature
        | MetricId::DiskTemperature
        | MetricId::BatteryTemperature => 2.0,
        MetricId::FanSpeed => 100.0,
        _ => 3.0,
    }
}

/// Build the status-item columns for every enabled metric, in user order.
pub fn build(
    reading: &ResidentReading,
    prefs: &ResidentPrefs,
    memory: &mut ToneMemory,
) -> Vec<Column> {
    let compact = prefs.compact;
    prefs
        .enabled_metrics()
        .into_iter()
        .map(|id| column(id, reading, prefs, compact, memory))
        .collect()
}

/// One column for `id`.
fn column(
    id: MetricId,
    reading: &ResidentReading,
    prefs: &ResidentPrefs,
    compact: bool,
    memory: &mut ToneMemory,
) -> Column {
    match id {
        MetricId::Cpu => usage(id, "CPU", reading.cpu.usage, prefs, compact, memory),
        MetricId::Gpu => usage(id, "GPU", reading.gpu.usage, prefs, compact, memory),
        MetricId::Memory => usage(
            id,
            "MEM",
            reading.memory.usage_percent,
            prefs,
            compact,
            memory,
        ),
        MetricId::Disk => usage(
            id,
            "DISK",
            reading.disk.used_percent,
            prefs,
            compact,
            memory,
        ),
        MetricId::CpuTemperature => {
            temperature(id, "CPU", reading.cpu.temp_c, prefs, compact, memory)
        }
        MetricId::GpuTemperature => {
            temperature(id, "GPU", reading.gpu.temp_c, prefs, compact, memory)
        }
        MetricId::DiskTemperature => {
            temperature(id, "DISK", reading.disk.temp_c, prefs, compact, memory)
        }
        MetricId::BatteryTemperature => temperature(
            id,
            "BAT",
            reading.power.battery_temp_c,
            prefs,
            compact,
            memory,
        ),
        MetricId::ChargingPower => power(
            id,
            "CHG",
            None,
            reading.power.charging_watts,
            prefs,
            compact,
            memory,
        ),
        MetricId::SystemPower => power(
            id,
            "SYS",
            None,
            reading.power.system_watts,
            prefs,
            compact,
            memory,
        ),
        MetricId::BatteryPower => {
            // Positive = discharging, negative = charging; a near-zero flow
            // is shown as IDLE so the arrow never flips on sensor noise.
            let watts = reading.power.battery_watts;
            let (heading, arrow) = match watts {
                Some(w) if w >= 0.5 => ("BAT", Some(Arrow::Up)),
                Some(w) if w <= -0.5 => ("BAT", Some(Arrow::Down)),
                Some(_) => ("IDLE", None),
                None => ("BAT", None),
            };
            power(
                id,
                heading,
                arrow,
                watts.map(f32::abs),
                prefs,
                compact,
                memory,
            )
        }
        MetricId::FanSpeed => fan(reading, prefs, compact, memory),
        MetricId::Network => network(reading, compact),
    }
}

/// Percentage column (CPU/GPU/MEM/DISK). Width reserves "100%" at 12 pt.
fn usage(
    id: MetricId,
    heading: &str,
    value: Option<f32>,
    prefs: &ResidentPrefs,
    compact: bool,
    memory: &mut ToneMemory,
) -> Column {
    let shown = value
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 100.0).round());
    stacked(
        id,
        heading,
        None,
        shown,
        |v| format!("{v:.0}%"),
        prefs,
        memory,
        if compact { 34 } else { 36 },
    )
}

/// Temperature column ("CPU" / "54°C").
fn temperature(
    id: MetricId,
    heading: &str,
    value: Option<f32>,
    prefs: &ResidentPrefs,
    compact: bool,
    memory: &mut ToneMemory,
) -> Column {
    let shown = value
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.round());
    stacked(
        id,
        heading,
        None,
        shown,
        |v| format!("{v:.0}°C"),
        prefs,
        memory,
        if compact { 37 } else { 46 },
    )
}

/// Power column ("SYS" / "12W"). Sub-watt readings show "<1W" and color as
/// zero; the field widens only when a fourth digit is actually needed.
fn power(
    id: MetricId,
    heading: &str,
    arrow: Option<Arrow>,
    watts: Option<f32>,
    prefs: &ResidentPrefs,
    compact: bool,
    memory: &mut ToneMemory,
) -> Column {
    let watts = watts.filter(|w| w.is_finite() && *w >= 0.0);
    let text = watts.map(power_digits);
    let wide = text.as_ref().is_some_and(|t| t.len() >= 4);
    let width = match (wide, compact) {
        (true, true) => 41,
        (true, false) => 45,
        (false, true) => 34,
        (false, false) => 37,
    };
    let (value, tone) = match (watts, text) {
        (Some(w), Some(t)) => {
            let colored = if w < 1.0 { 0.0 } else { w.round() };
            (format!("{t}W"), memory.tone(prefs, id, 0, colored))
        }
        _ => {
            memory.clear(id, 0);
            (MISSING.to_string(), Tone::Normal)
        }
    };
    Column {
        id,
        width,
        body: Body::Stacked {
            heading: heading.to_string(),
            arrow,
            value,
            tone,
        },
    }
}

/// Watt digits: "<1" for a non-zero sub-watt flow, else the rounded value.
fn power_digits(watts: f32) -> String {
    if watts > 0.0 && watts < 1.0 {
        "<1".into()
    } else {
        format!("{:.0}", watts.round())
    }
}

/// Shared heading-over-value column with a toned value or a dash.
#[allow(clippy::too_many_arguments)]
fn stacked(
    id: MetricId,
    heading: &str,
    arrow: Option<Arrow>,
    shown: Option<f32>,
    format: impl Fn(f32) -> String,
    prefs: &ResidentPrefs,
    memory: &mut ToneMemory,
    width: u32,
) -> Column {
    let (value, tone) = match shown {
        Some(v) => (format(v), memory.tone(prefs, id, 0, v)),
        None => {
            memory.clear(id, 0);
            (MISSING.to_string(), Tone::Normal)
        }
    };
    Column {
        id,
        width,
        body: Body::Stacked {
            heading: heading.to_string(),
            arrow,
            value,
            tone,
        },
    }
}

/// Fan column: highest / average RPM, one selected fan, or two selected fans
/// as two rows ("F1 1200" / "F2 1180"). Fan ids are 0-based; labels 1-based.
fn fan(
    reading: &ResidentReading,
    prefs: &ResidentPrefs,
    compact: bool,
    memory: &mut ToneMemory,
) -> Column {
    let id = MetricId::FanSpeed;
    let rpm_of = |fan_id: u32| reading.fans.iter().find(|f| f.id == fan_id).map(|f| f.rpm);
    let rpms: Vec<f32> = reading
        .fans
        .iter()
        .map(|f| f.rpm)
        .filter(|r| r.is_finite())
        .collect();
    let single = |heading: String, rpm: Option<f32>, memory: &mut ToneMemory| {
        stacked(
            id,
            &heading,
            None,
            rpm.filter(|r| r.is_finite() && *r >= 0.0).map(f32::round),
            |v| format!("{v:.0}"),
            prefs,
            memory,
            if compact { 38 } else { 42 },
        )
    };
    match (prefs.fan_display, prefs.fan_ids.as_slice()) {
        (FanDisplay::Selected, [a, b, ..]) => {
            let mut row = |index: u8, fan_id: u32| match rpm_of(fan_id).filter(|r| r.is_finite()) {
                Some(rpm) => {
                    let rpm = rpm.max(0.0).round();
                    (
                        format!("F{} {rpm:.0}", fan_id + 1),
                        memory.tone(prefs, id, index, rpm),
                    )
                }
                None => {
                    memory.clear(id, index);
                    (format!("F{} {MISSING}", fan_id + 1), Tone::Normal)
                }
            };
            let rows = [row(0, *a), row(1, *b)];
            Column {
                id,
                width: if compact { 58 } else { 64 },
                body: Body::Rows(rows),
            }
        }
        (FanDisplay::Selected, [a]) => single(format!("FAN {}", a + 1), rpm_of(*a), memory),
        (FanDisplay::Average, _) => {
            let avg = (!rpms.is_empty()).then(|| rpms.iter().sum::<f32>() / rpms.len() as f32);
            single("FAVG".into(), avg, memory)
        }
        _ => single("FMAX".into(), rpms.iter().copied().reduce(f32::max), memory),
    }
}

/// Network column: upload over download, three digits plus a unit.
fn network(reading: &ResidentReading, compact: bool) -> Column {
    let layout = rate_layout(compact);
    Column {
        id: MetricId::Network,
        width: layout.unit_offset + layout.unit_width,
        body: Body::Rates {
            up: rate(reading.network.tx_bps, compact),
            down: rate(reading.network.rx_bps, compact),
        },
    }
}

/// (digits, unit) for a byte rate, 1024-based, capped at three digits so the
/// reserved field always fits; compact mode abbreviates units to one letter.
fn rate(bps: Option<u64>, compact: bool) -> (String, String) {
    let Some(bps) = bps else {
        return (MISSING.to_string(), String::new());
    };
    const UNITS: [(&str, &str); 5] = [
        ("B", "B/s"),
        ("K", "KB/s"),
        ("M", "MB/s"),
        ("G", "GB/s"),
        ("T", "TB/s"),
    ];
    let mut number = bps as f64;
    let mut unit = 0;
    // Promote once the rounded value would need a fourth digit, so the
    // three-digit field never truncates (1000 B/s shows as "1 KB/s").
    while number >= 999.5 && unit < UNITS.len() - 1 {
        number /= 1024.0;
        unit += 1;
    }
    let (short, long) = UNITS[unit];
    (
        format!("{:.0}", number.min(999.0)),
        if compact { short } else { long }.to_string(),
    )
}

/// One-line plain-text summary for the tooltip / accessibility label,
/// e.g. "CPU 37% · MEM 52% · ↑ 1 KB/s ↓ 3 MB/s".
pub fn summary(columns: &[Column]) -> String {
    columns
        .iter()
        .map(|column| match &column.body {
            Body::Stacked {
                heading,
                arrow,
                value,
                ..
            } => {
                let arrow = match arrow {
                    Some(Arrow::Up) => "↑",
                    Some(Arrow::Down) => "↓",
                    None => "",
                };
                format!("{heading}{arrow} {value}")
            }
            Body::Rows([a, b]) => format!("{} / {}", a.0, b.0),
            Body::Rates { up, down } => format!("↑ {} {} ↓ {} {}", up.0, up.1, down.0, down.1)
                .trim()
                .to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mole_ops::resident::prefs::MetricEntry;
    use mole_ops::resident::FanReading;

    /// Prefs with exactly `ids` enabled, in that order.
    fn prefs_with(ids: &[MetricId]) -> ResidentPrefs {
        let mut prefs = ResidentPrefs::default();
        for entry in &mut prefs.metrics {
            entry.enabled = false;
        }
        let mut ordered: Vec<MetricEntry> = ids
            .iter()
            .map(|id| {
                let mut e = prefs.metrics.iter().find(|m| m.id == *id).unwrap().clone();
                e.enabled = true;
                e
            })
            .collect();
        prefs.metrics.retain(|m| !ids.contains(&m.id));
        ordered.append(&mut prefs.metrics);
        prefs.metrics = ordered;
        prefs
    }

    #[test]
    fn columns_follow_user_order_and_fixed_widths() {
        let prefs = prefs_with(&[MetricId::Memory, MetricId::Cpu, MetricId::Network]);
        let mut reading = ResidentReading::default();
        reading.cpu.usage = Some(7.0);
        reading.memory.usage_percent = Some(100.0);
        let cols = build(&reading, &prefs, &mut ToneMemory::default());
        let ids: Vec<_> = cols.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            vec![MetricId::Memory, MetricId::Cpu, MetricId::Network]
        );
        // Digits never change the width.
        assert_eq!(cols[0].width, cols[1].width);
        assert_eq!(width(&cols, true, false), 18 + 36 + 36 + 70 + 6 * 3);
        let compact = ResidentPrefs {
            compact: true,
            ..prefs
        };
        let cols = build(&reading, &compact, &mut ToneMemory::default());
        assert_eq!(width(&cols, false, true), 34 + 34 + 42 + 2 * 2);
    }

    #[test]
    fn missing_values_render_a_dash_in_normal_tone() {
        let prefs = prefs_with(&[MetricId::Cpu, MetricId::CpuTemperature]);
        let cols = build(
            &ResidentReading::default(),
            &prefs,
            &mut ToneMemory::default(),
        );
        for col in cols {
            match col.body {
                Body::Stacked { value, tone, .. } => {
                    assert_eq!(value, "—");
                    assert_eq!(tone, Tone::Normal);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn descending_boundary_holds_previous_tone_within_margin() {
        let prefs = prefs_with(&[MetricId::Cpu]);
        let mut memory = ToneMemory::default();
        assert_eq!(memory.tone(&prefs, MetricId::Cpu, 0, 92.0), Tone::Critical);
        // 88 is "warning" but within 3 points of the 90 boundary: hold.
        assert_eq!(memory.tone(&prefs, MetricId::Cpu, 0, 88.0), Tone::Critical);
        assert_eq!(memory.tone(&prefs, MetricId::Cpu, 0, 86.0), Tone::Warning);
        // Rising switches immediately.
        assert_eq!(memory.tone(&prefs, MetricId::Cpu, 0, 90.0), Tone::Critical);
    }

    #[test]
    fn network_rates_pick_units_and_cap_digits() {
        assert_eq!(rate(Some(0), false), ("0".into(), "B/s".into()));
        assert_eq!(rate(Some(1023), false), ("1".into(), "KB/s".into()));
        assert_eq!(rate(Some(5 * 1024 * 1024), true), ("5".into(), "M".into()));
        assert_eq!(rate(Some(u64::MAX), false).0, "999");
        assert_eq!(rate(None, false), ("—".into(), String::new()));
    }

    #[test]
    fn battery_direction_and_power_width() {
        let prefs = prefs_with(&[MetricId::BatteryPower, MetricId::SystemPower]);
        let mut reading = ResidentReading::default();
        reading.power.battery_watts = Some(-12.4);
        reading.power.system_watts = Some(1234.0);
        let cols = build(&reading, &prefs, &mut ToneMemory::default());
        match &cols[0].body {
            Body::Stacked {
                heading,
                arrow,
                value,
                ..
            } => {
                assert_eq!(
                    (heading.as_str(), *arrow, value.as_str()),
                    ("BAT", Some(Arrow::Down), "12W")
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(cols[0].width, 37);
        assert_eq!(cols[1].width, 45);
        reading.power.battery_watts = Some(0.2);
        let cols = build(&reading, &prefs, &mut ToneMemory::default());
        assert!(
            matches!(&cols[0].body, Body::Stacked { heading, arrow: None, .. } if heading == "IDLE")
        );
    }

    #[test]
    fn fan_modes() {
        let mut prefs = prefs_with(&[MetricId::FanSpeed]);
        let reading = ResidentReading {
            fans: vec![
                FanReading { id: 0, rpm: 1200.4 },
                FanReading { id: 1, rpm: 1800.0 },
            ],
            ..ResidentReading::default()
        };
        let cols = build(&reading, &prefs, &mut ToneMemory::default());
        assert!(
            matches!(&cols[0].body, Body::Stacked { heading, value, .. } if heading == "FMAX" && value == "1800")
        );
        prefs.fan_display = FanDisplay::Selected;
        prefs.fan_ids = vec![0, 1];
        let cols = build(&reading, &prefs, &mut ToneMemory::default());
        assert_eq!(cols[0].width, 64);
        match &cols[0].body {
            Body::Rows([a, b]) => assert_eq!((a.0.as_str(), b.0.as_str()), ("F1 1200", "F2 1800")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn summary_reads_like_the_columns() {
        let prefs = prefs_with(&[MetricId::Cpu, MetricId::Network]);
        let mut reading = ResidentReading::default();
        reading.cpu.usage = Some(37.2);
        reading.network.tx_bps = Some(2048);
        reading.network.rx_bps = Some(10);
        let cols = build(&reading, &prefs, &mut ToneMemory::default());
        assert_eq!(summary(&cols), "CPU 37% · ↑ 2 KB/s ↓ 10 B/s");
    }
}
