// Battery and power rails. Battery state comes from the AppleSmartBattery
// registry entry (percent, charging flags, pack voltage × current); whole
// system and adapter power from read-only SMC sensors (PSTR = system total,
// PDTR = DC-in, PPBR = battery rail) and pack temperature from TB1T/TB2T.
// Desktops have no AppleSmartBattery: battery fields stay None, but system
// power may still be reported.

use super::ffi::IoObject;
use super::PowerReading;
use crate::smc;

/// Highest plausible watt figure for a Mac rail; anything above is a misread.
const MAX_WATTS: f32 = 500.0;

/// Keep only finite, in-range watt values.
fn sane_watts(v: Option<f32>) -> Option<f32> {
    v.filter(|w| w.is_finite() && (0.0..=MAX_WATTS).contains(w))
}

/// Raw battery registry values.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct BatteryRaw {
    pub current_capacity: Option<i64>,
    pub max_capacity: Option<i64>,
    pub is_charging: Option<bool>,
    pub external: Option<bool>,
    /// mV
    pub voltage: Option<i64>,
    /// mA, signed: negative = discharging.
    pub amperage: Option<i64>,
    /// centi-°C (registry "Temperature").
    pub temperature: Option<i64>,
}

/// Read the AppleSmartBattery entry; None on machines without a battery.
fn read_battery() -> Option<BatteryRaw> {
    let b = IoObject::service(c"AppleSmartBattery")?;
    if b.boolean("BatteryInstalled") == Some(false) {
        return None;
    }
    Some(BatteryRaw {
        current_capacity: b.int("CurrentCapacity"),
        max_capacity: b.int("MaxCapacity"),
        is_charging: b.boolean("IsCharging"),
        external: b.boolean("ExternalConnected"),
        voltage: b.int("Voltage"),
        amperage: b.int("InstantAmperage").or_else(|| b.int("Amperage")),
        temperature: b.int("Temperature"),
    })
}

/// Battery percent: Apple Silicon reports CurrentCapacity already as 0–100
/// with MaxCapacity 100; Intel reports mAh for both, so always divide.
pub(crate) fn percent(raw: &BatteryRaw) -> Option<u8> {
    let cur = raw.current_capacity?;
    let max = raw.max_capacity?;
    (max > 0 && cur >= 0).then(|| ((cur as f64 / max as f64) * 100.0).round().min(100.0) as u8)
}

/// Pack power, watts, signed like the contract: positive = discharging,
/// negative = charging. Voltage × current from the registry; when on battery
/// the SMC battery rail (PPBR) reacts faster than the pack current, which
/// can read 0 for a minute after unplugging.
pub(crate) fn battery_watts(raw: &BatteryRaw, ppbr: Option<f32>) -> Option<f32> {
    let external = raw.external.unwrap_or(false);
    if !external {
        if let Some(w) = sane_watts(ppbr).filter(|w| *w > 0.0) {
            return Some(w);
        }
    }
    let (mv, ma) = (raw.voltage?, raw.amperage?);
    if !(1..=100_000).contains(&mv) || ma.abs() > 100_000 {
        return None;
    }
    // Registry current is negative while discharging; flip to our sign.
    let w = -(mv as f64 * ma as f64 / 1_000_000.0) as f32;
    // An idle pack reads 0 mA; never publish "-0".
    let w = if w == 0.0 { 0.0 } else { w };
    (w.abs() <= MAX_WATTS).then_some(w)
}

/// Battery temperature: mean of the SMC pack sensors, else the registry's
/// centi-degree value.
pub(crate) fn battery_temp(smc_values: &[Option<f32>], registry: Option<i64>) -> Option<f32> {
    let vals: Vec<f32> = smc_values
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 100.0)
        .collect();
    if !vals.is_empty() {
        return Some(vals.iter().sum::<f32>() / vals.len() as f32);
    }
    registry
        .map(|c| c as f32 / 100.0)
        .filter(|v| *v > 0.0 && *v <= 100.0)
}

/// One power observation.
pub(crate) fn read() -> PowerReading {
    let battery = read_battery();
    let system_watts = sane_watts(smc::read_float("PSTR"));
    let Some(raw) = battery else {
        return PowerReading {
            system_watts,
            ..PowerReading::default()
        };
    };
    let external = raw.external.unwrap_or(false);
    // Adapter input: SMC DC-in rail while plugged in; a real zero on battery.
    let charging_watts = if external {
        sane_watts(smc::read_float("PDTR"))
    } else {
        Some(0.0)
    };
    let ppbr = if external {
        None
    } else {
        smc::read_float("PPBR")
    };
    PowerReading {
        battery_percent: percent(&raw),
        charging: raw.is_charging.unwrap_or(false) && external,
        charging_watts,
        battery_watts: battery_watts(&raw, ppbr),
        system_watts,
        battery_temp_c: battery_temp(
            &[smc::read_float("TB1T"), smc::read_float("TB2T")],
            raw.temperature,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw() -> BatteryRaw {
        BatteryRaw {
            current_capacity: Some(80),
            max_capacity: Some(100),
            is_charging: Some(false),
            external: Some(false),
            voltage: Some(12_000),
            amperage: Some(-1_000),
            temperature: Some(3050),
        }
    }

    #[test]
    fn percent_handles_both_capacity_units() {
        assert_eq!(percent(&raw()), Some(80));
        let intel = BatteryRaw {
            current_capacity: Some(3000),
            max_capacity: Some(6000),
            ..raw()
        };
        assert_eq!(percent(&intel), Some(50));
        let bad = BatteryRaw {
            max_capacity: Some(0),
            ..raw()
        };
        assert_eq!(percent(&bad), None);
    }

    #[test]
    fn battery_watts_sign_and_sources() {
        // Discharging from the registry: +12 W.
        assert_eq!(battery_watts(&raw(), None), Some(12.0));
        // On battery, PPBR wins when present.
        assert_eq!(battery_watts(&raw(), Some(9.5)), Some(9.5));
        // Charging: positive pack current → negative watts; PPBR ignored.
        let charging = BatteryRaw {
            external: Some(true),
            is_charging: Some(true),
            amperage: Some(2_000),
            ..raw()
        };
        assert_eq!(battery_watts(&charging, Some(9.5)), Some(-24.0));
        // Garbage voltage → None.
        let bad = BatteryRaw {
            voltage: Some(0),
            ..raw()
        };
        assert_eq!(battery_watts(&bad, None), None);
        let idle = BatteryRaw {
            external: Some(true),
            amperage: Some(0),
            ..raw()
        };
        assert!(battery_watts(&idle, None).unwrap().is_sign_positive());
    }

    #[test]
    fn battery_temp_prefers_smc_mean() {
        assert_eq!(
            battery_temp(&[Some(30.0), Some(32.0)], Some(2500)),
            Some(31.0)
        );
        assert_eq!(battery_temp(&[None, Some(0.0)], Some(2500)), Some(25.0));
        assert_eq!(battery_temp(&[], None), None);
    }

    /// Smoke: never panics; any reported numbers are sane.
    #[test]
    fn live_read_is_sane() {
        let p = read();
        if let Some(pct) = p.battery_percent {
            assert!(pct <= 100);
        }
        for w in [p.charging_watts, p.system_watts].into_iter().flatten() {
            assert!((0.0..=MAX_WATTS).contains(&w));
        }
    }
}
