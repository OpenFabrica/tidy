// SMC (System Management Controller) client: fan and die temperature reads,
// plus the fan-override writes the privileged helper uses.
// Reading SMC keys via the AppleSMC IOKit user client needs no privileges.
// Writing does (the kernel refuses non-root), so the write path additionally
// refuses unless the process is root — the GUI can never flip a fan by
// accident. Every read failure returns None/empty so the dashboard shows "—"
// rather than a guessed number.

use serde::Serialize;
use std::ffi::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

/// One fan's telemetry, all in RPM. `actual_rpm` is always present; the
/// bounds/target keys are optional because some models omit them.
#[derive(Debug, Clone, Serialize)]
pub struct FanStatus {
    pub actual_rpm: f32,
    pub min_rpm: Option<f32>,
    pub max_rpm: Option<f32>,
    pub target_rpm: Option<f32>,
}

/// Hottest CPU and GPU die sensors in °C. None when the SMC exposes no
/// matching key (VM/CI) or every reading is out of the sane range.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Temperatures {
    pub cpu_c: Option<f32>,
    pub gpu_c: Option<f32>,
}

// AppleSMC user-client ABI (stable since Intel; unchanged on Apple Silicon).
const KERNEL_INDEX_SMC: u32 = 2;
const SMC_CMD_READ_BYTES: u8 = 5;
const SMC_CMD_READ_KEYINFO: u8 = 9;
const SMC_CMD_READ_INDEX: u8 = 8;
const SMC_CMD_WRITE_BYTES: u8 = 6;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcVers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcPLimit {
    version: u16,
    length: u16,
    cpu_plimit: u32,
    gpu_plimit: u32,
    mem_plimit: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcKeyInfo {
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
}

/// The 80-byte in/out struct AppleSMC's kSMCHandleYPCEvent selector expects.
/// Field order and C padding must match the kernel's layout exactly.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcKeyData {
    key: u32,
    vers: SmcVers,
    p_limit: SmcPLimit,
    key_info: SmcKeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, conn_type: u32, connect: *mut u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
}

extern "C" {
    fn mach_task_self() -> u32;
}

/// Process-lifetime AppleSMC connection, opened once and mutex-guarded
/// (the user client is not documented as concurrency-safe). None means the
/// service is absent (VM/CI) or open failed — callers then report no fans.
fn connection() -> Option<&'static Mutex<u32>> {
    static CONN: OnceLock<Option<Mutex<u32>>> = OnceLock::new();
    CONN.get_or_init(|| {
        // Matching dictionary is consumed by IOServiceGetMatchingService.
        let service =
            unsafe { IOServiceGetMatchingService(0, IOServiceMatching(c"AppleSMC".as_ptr())) };
        if service == 0 {
            return None;
        }
        let mut conn: u32 = 0;
        let rc = unsafe { IOServiceOpen(service, mach_task_self(), 0, &mut conn) };
        unsafe { IOObjectRelease(service) };
        (rc == 0 && conn != 0).then(|| Mutex::new(conn))
    })
    .as_ref()
}

/// One round-trip through the SMC selector; None on any kernel error.
fn call(conn: u32, input: &SmcKeyData) -> Option<SmcKeyData> {
    let mut out = SmcKeyData::default();
    let mut out_size = std::mem::size_of::<SmcKeyData>();
    let rc = unsafe {
        IOConnectCallStructMethod(
            conn,
            KERNEL_INDEX_SMC,
            (input as *const SmcKeyData).cast(),
            std::mem::size_of::<SmcKeyData>(),
            (&mut out as *mut SmcKeyData).cast(),
            &mut out_size,
        )
    };
    (rc == 0).then_some(out)
}

/// Four-character SMC key as the big-endian u32 the ABI uses.
fn fourcc(key: &str) -> Option<u32> {
    let bytes: [u8; 4] = key.as_bytes().try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// Read one key: (type fourcc, raw bytes). Two round-trips — key info for
/// size/type, then the payload — because size is not knowable up front.
fn read_key(conn: u32, key: &str) -> Option<(u32, Vec<u8>)> {
    let key = fourcc(key)?;
    let mut info_req = SmcKeyData {
        key,
        data8: SMC_CMD_READ_KEYINFO,
        ..Default::default()
    };
    let info = call(conn, &info_req)?;
    let size = info.key_info.data_size;
    if info.result != 0 || size == 0 || size > 32 {
        return None;
    }
    info_req.data8 = SMC_CMD_READ_BYTES;
    info_req.key_info.data_size = size;
    let data = call(conn, &info_req)?;
    if data.result != 0 {
        return None;
    }
    Some((
        info.key_info.data_type,
        data.bytes[..size as usize].to_vec(),
    ))
}

/// Key name at enumeration index `i` (0..#KEY). One round-trip; None on error.
fn key_at_index(conn: u32, i: u32) -> Option<String> {
    let req = SmcKeyData {
        data8: SMC_CMD_READ_INDEX,
        data32: i,
        ..Default::default()
    };
    let out = call(conn, &req)?;
    if out.result != 0 {
        return None;
    }
    String::from_utf8(out.key.to_be_bytes().to_vec()).ok()
}

/// Every key the SMC exposes, enumerated once per process (thousands of
/// round-trips on Apple Silicon, so callers must cache what they derive).
fn all_keys(conn: u32) -> Vec<String> {
    let count = read_key(conn, "#KEY")
        .and_then(|(ty, bytes)| decode_uint(ty, &bytes))
        .unwrap_or(0)
        .min(16_384) as u32;
    (0..count).filter_map(|i| key_at_index(conn, i)).collect()
}

/// Debug probe: (key, type, decoded value) for every key starting with
/// `prefix`. Used by the `smc_dump` example to map sensor names on new chips.
pub fn dump_keys(prefix: &str) -> Vec<(String, String, Option<f32>)> {
    let Some(lock) = connection() else {
        return Vec::new();
    };
    let Ok(conn) = lock.lock() else {
        return Vec::new();
    };
    let conn = *conn;
    all_keys(conn)
        .into_iter()
        .filter(|k| k.starts_with(prefix))
        .filter_map(|k| {
            let (ty, bytes) = read_key(conn, &k)?;
            let ty_s = String::from_utf8_lossy(&ty.to_be_bytes()).to_string();
            Some((k, ty_s, decode_float(ty, &bytes)))
        })
        .collect()
}

// Apple Silicon die-sensor key prefixes. Apple never documented them; these
// come from probing M1–M5 with `smc_dump`: "Tp" = CPU performance/efficiency
// cores (M1/M2/M4/M5), "Te"/"Tf" = CPU cores on M3, "Tg" = GPU cores. Every
// other "T" key is a board/ambient/battery sensor and must stay excluded.
const CPU_TEMP_PREFIXES: [&str; 3] = ["Tp", "Te", "Tf"];
const GPU_TEMP_PREFIXES: [&str; 1] = ["Tg"];

/// CPU and GPU sensor key names, enumerated once per process because the
/// full key walk is ~3.5k round-trips; the sets never change while running.
fn temperature_keys(conn: u32) -> &'static (Vec<String>, Vec<String>) {
    static KEYS: OnceLock<(Vec<String>, Vec<String>)> = OnceLock::new();
    KEYS.get_or_init(|| {
        let keys = all_keys(conn);
        let pick = |prefixes: &[&str]| -> Vec<String> {
            keys.iter()
                .filter(|k| prefixes.iter().any(|p| k.starts_with(p)))
                .cloned()
                .collect()
        };
        (pick(&CPU_TEMP_PREFIXES), pick(&GPU_TEMP_PREFIXES))
    })
}

/// Hottest reading across `keys` in °C. Unpopulated sensors report 0.0 and a
/// misdecoded value can be huge, so only 1–125 °C counts as a real die reading.
fn max_temperature(conn: u32, keys: &[String]) -> Option<f32> {
    keys.iter()
        .filter_map(|k| {
            let (ty, bytes) = read_key(conn, k)?;
            decode_float(ty, &bytes)
        })
        .filter(|v| v.is_finite() && (1.0..=125.0).contains(v))
        .reduce(f32::max)
}

/// Hottest CPU and GPU die temperatures via unprivileged SMC reads. Both are
/// None inside VMs, on Intel Macs (different key scheme, not mapped), or on
/// any SMC error — the dashboard then omits the reading rather than guess.
pub fn temperatures() -> Temperatures {
    let Some(lock) = connection() else {
        return Temperatures::default();
    };
    let Ok(conn) = lock.lock() else {
        return Temperatures::default();
    };
    let conn = *conn;
    let (cpu_keys, gpu_keys) = temperature_keys(conn);
    Temperatures {
        cpu_c: max_temperature(conn, cpu_keys),
        gpu_c: max_temperature(conn, gpu_keys),
    }
}

/// One SMC key decoded as f32 (power rails, battery sensors) for the
/// resident sampler. None when the key is absent or the SMC is unavailable.
pub(crate) fn read_float(key: &str) -> Option<f32> {
    let lock = connection()?;
    let conn = *lock.lock().ok()?;
    let (ty, bytes) = read_key(conn, key)?;
    decode_float(ty, &bytes).filter(|v| v.is_finite())
}

/// Decode an unsigned SMC integer (ui8/ui16/ui32, big-endian).
fn decode_uint(data_type: u32, bytes: &[u8]) -> Option<u64> {
    match (&data_type.to_be_bytes(), bytes.len()) {
        (b"ui8 ", 1) => Some(u64::from(bytes[0])),
        (b"ui16", 2) => Some(u64::from(u16::from_be_bytes([bytes[0], bytes[1]]))),
        (b"ui32", 4) => Some(u64::from(u32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))),
        _ => None,
    }
}

/// Decode an SMC number as f32. Apple Silicon reports fans as IEEE `flt `
/// (little-endian); older Intel firmware uses `fpe2` fixed-point (big-endian,
/// two fraction bits). Integer types pass through for robustness.
fn decode_float(data_type: u32, bytes: &[u8]) -> Option<f32> {
    match (&data_type.to_be_bytes(), bytes.len()) {
        (b"flt ", 4) => Some(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        (b"fpe2", 2) => Some(f32::from(u16::from_be_bytes([bytes[0], bytes[1]])) / 4.0),
        _ => decode_uint(data_type, bytes).map(|v| v as f32),
    }
}

/// Read one fan-scoped float key like "F0Ac", rejecting NaN/negatives so a
/// misdecoded value can never reach the UI.
fn read_fan_float(conn: u32, index: u64, suffix: &str) -> Option<f32> {
    let (ty, bytes) = read_key(conn, &format!("F{index}{suffix}"))?;
    decode_float(ty, &bytes).filter(|v| v.is_finite() && *v >= 0.0)
}

/// All fans' telemetry. Empty on fanless models (MacBook Air), inside VMs,
/// or on any SMC error — the caller renders that as "no fans", not zero RPM.
pub fn fans() -> Vec<FanStatus> {
    let Some(lock) = connection() else {
        return Vec::new();
    };
    let Ok(conn) = lock.lock() else {
        return Vec::new();
    };
    let conn = *conn;
    let count = read_key(conn, "FNum")
        .and_then(|(ty, bytes)| decode_uint(ty, &bytes))
        .unwrap_or(0)
        // Real Macs top out at 2 fans; a larger count means garbage data.
        .min(8);
    (0..count)
        .filter_map(|i| {
            Some(FanStatus {
                actual_rpm: read_fan_float(conn, i, "Ac")?,
                min_rpm: read_fan_float(conn, i, "Mn"),
                max_rpm: read_fan_float(conn, i, "Mx"),
                target_rpm: read_fan_float(conn, i, "Tg"),
            })
        })
        .collect()
}

/// Write raw bytes to one key. The caller must have already read the key's
/// info to encode `bytes` in the key's own type; the size must match exactly.
fn write_key(conn: u32, key: &str, bytes: &[u8]) -> Result<(), String> {
    let key_id = fourcc(key).ok_or_else(|| format!("bad key {key}"))?;
    let (_, existing) = read_key(conn, key).ok_or_else(|| format!("{key} not readable"))?;
    if existing.len() != bytes.len() {
        return Err(format!(
            "{key}: size mismatch ({} vs {})",
            existing.len(),
            bytes.len()
        ));
    }
    let mut req = SmcKeyData {
        key: key_id,
        data8: SMC_CMD_WRITE_BYTES,
        ..Default::default()
    };
    req.key_info.data_size = bytes.len() as u32;
    req.bytes[..bytes.len()].copy_from_slice(bytes);
    let out = call(conn, &req).ok_or_else(|| format!("{key}: SMC call failed"))?;
    if out.result != 0 {
        return Err(format!("{key}: SMC write refused (result {})", out.result));
    }
    Ok(())
}

/// Encode an f32 in the key's own numeric type (mirror of `decode_float`).
fn encode_float(data_type: u32, value: f32) -> Option<Vec<u8>> {
    match &data_type.to_be_bytes() {
        b"flt " => Some(value.to_le_bytes().to_vec()),
        b"fpe2" => Some(((value * 4.0).round() as u16).to_be_bytes().to_vec()),
        b"ui16" => Some((value.round() as u16).to_be_bytes().to_vec()),
        b"ui8 " => Some(vec![value.round() as u8]),
        _ => None,
    }
}

/// Root check for the write path: the kernel refuses non-root SMC writes
/// anyway, but failing early keeps the error message honest.
fn ensure_root() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("SMC writes require root".into());
    }
    if std::env::var("MOLE_TEST_MODE").as_deref() == Ok("1") {
        return Err("blocked in test mode".into());
    }
    Ok(())
}

/// The fan-mode key name: Intel firmware spells it "F0Md", Apple Silicon
/// "F0md". Whichever the SMC answers for is the real one.
fn fan_mode_key(conn: u32, index: u64) -> Option<String> {
    ["Md", "md"]
        .iter()
        .map(|suffix| format!("F{index}{suffix}"))
        .find(|k| read_key(conn, k).is_some())
}

/// Clamp a requested RPM into the fan's own [min, max] band. Public so the
/// helper's request vetting and the UI slider agree on the rule.
pub fn clamp_rpm(rpm: f32, min: Option<f32>, max: Option<f32>) -> Option<f32> {
    if !rpm.is_finite() {
        return None;
    }
    // Without a known ceiling the fan must not be forced at all.
    let max = max.filter(|m| m.is_finite() && *m > 0.0)?;
    let min = min.filter(|m| m.is_finite() && *m >= 0.0).unwrap_or(0.0);
    Some(rpm.clamp(min, max))
}

/// Force every fan to `rpm` (clamped per fan into its min/max band), i.e.
/// switch it to manual mode and set the target. Returns the RPM actually
/// applied to the first fan. Root only; see `set_fans_auto` for the undo.
pub fn set_fans_forced(rpm: f32) -> Result<f32, String> {
    ensure_root()?;
    let lock = connection().ok_or("SMC unavailable")?;
    let conn = *lock.lock().map_err(|_| "SMC lock poisoned")?;
    let mut applied = None;
    for (i, fan) in fans().iter().enumerate() {
        let i = i as u64;
        let target = clamp_rpm(rpm, fan.min_rpm, fan.max_rpm)
            .ok_or_else(|| format!("fan {i}: unknown RPM range"))?;
        let mode_key = fan_mode_key(conn, i).ok_or_else(|| format!("fan {i}: no mode key"))?;
        // Order matters: firmware ignores a target written while in auto mode.
        write_key(conn, &mode_key, &[1])?;
        let tg = format!("F{i}Tg");
        let (ty, _) = read_key(conn, &tg).ok_or_else(|| format!("{tg} unreadable"))?;
        let bytes = encode_float(ty, target).ok_or_else(|| format!("{tg}: unsupported type"))?;
        write_key(conn, &tg, &bytes)?;
        applied.get_or_insert(target);
    }
    applied.ok_or_else(|| "no fans".to_string())
}

/// Hand every fan back to macOS (auto mode). Root only. Continues past
/// per-fan failures so one bad key never leaves another fan forced, and
/// reports the first error.
pub fn set_fans_auto() -> Result<(), String> {
    ensure_root()?;
    let lock = connection().ok_or("SMC unavailable")?;
    let conn = *lock.lock().map_err(|_| "SMC lock poisoned")?;
    let mut first_err = None;
    for i in 0..fans().len() as u64 {
        let res = fan_mode_key(conn, i)
            .ok_or_else(|| format!("fan {i}: no mode key"))
            .and_then(|k| write_key(conn, &k, &[0]));
        if let Err(e) = res {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// Whether any fan is currently in manual (forced) mode. Read-only, so it
/// works unprivileged; None when the SMC exposes no mode key.
pub fn fans_forced() -> Option<bool> {
    let lock = connection()?;
    let conn = *lock.lock().ok()?;
    let mut any_known = false;
    let mut forced = false;
    for i in 0..fans().len() as u64 {
        if let Some(k) = fan_mode_key(conn, i) {
            if let Some((ty, bytes)) = read_key(conn, &k) {
                any_known = true;
                forced |= decode_uint(ty, &bytes).unwrap_or(0) != 0;
            }
        }
    }
    any_known.then_some(forced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_float_roundtrips_decode() {
        let flt = u32::from_be_bytes(*b"flt ");
        let bytes = encode_float(flt, 2500.0).unwrap();
        assert_eq!(decode_float(flt, &bytes), Some(2500.0));
        let fpe2 = u32::from_be_bytes(*b"fpe2");
        let bytes = encode_float(fpe2, 1250.0).unwrap();
        assert_eq!(decode_float(fpe2, &bytes), Some(1250.0));
        assert_eq!(encode_float(u32::from_be_bytes(*b"ch8*"), 1.0), None);
    }

    #[test]
    fn clamp_rpm_stays_inside_the_fan_band_and_needs_a_ceiling() {
        assert_eq!(clamp_rpm(9000.0, Some(2300.0), Some(7800.0)), Some(7800.0));
        assert_eq!(clamp_rpm(100.0, Some(2300.0), Some(7800.0)), Some(2300.0));
        assert_eq!(clamp_rpm(4000.0, Some(2300.0), Some(7800.0)), Some(4000.0));
        assert_eq!(clamp_rpm(4000.0, None, Some(7800.0)), Some(4000.0));
        assert_eq!(clamp_rpm(4000.0, Some(2300.0), None), None);
        assert_eq!(clamp_rpm(f32::NAN, Some(2300.0), Some(7800.0)), None);
    }

    /// The write path must refuse for a non-root process before touching
    /// the SMC — this is what keeps the GUI read-only by construction.
    #[test]
    fn writes_refuse_without_root() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(set_fans_forced(3000.0).is_err());
        assert!(set_fans_auto().is_err());
    }

    /// The kernel ABI expects exactly 80 bytes; a padding drift here corrupts
    /// every field after it.
    #[test]
    fn smc_key_data_layout_is_80_bytes() {
        assert_eq!(std::mem::size_of::<SmcKeyData>(), 80);
    }

    #[test]
    fn decodes_flt_little_endian() {
        let ty = u32::from_be_bytes(*b"flt ");
        assert_eq!(decode_float(ty, &1234.5f32.to_le_bytes()), Some(1234.5));
    }

    #[test]
    fn decodes_fpe2_fixed_point() {
        let ty = u32::from_be_bytes(*b"fpe2");
        // 0x1388 = 5000 raw → 1250 RPM after the 2-bit shift.
        assert_eq!(decode_float(ty, &[0x13, 0x88]), Some(1250.0));
    }

    #[test]
    fn decodes_uints_and_rejects_mismatched_sizes() {
        assert_eq!(decode_uint(u32::from_be_bytes(*b"ui8 "), &[2]), Some(2));
        assert_eq!(
            decode_uint(u32::from_be_bytes(*b"ui16"), &[0x01, 0x00]),
            Some(256)
        );
        assert_eq!(decode_uint(u32::from_be_bytes(*b"ui8 "), &[1, 2]), None);
        assert_eq!(decode_float(u32::from_be_bytes(*b"ch8*"), &[1, 2]), None);
    }

    /// Smoke: never panics, and any reported die temperature is in the sane
    /// 1–125 °C band — on real hardware and on SMC-less CI VMs alike.
    #[test]
    fn temperatures_fail_closed() {
        let t = temperatures();
        for v in [t.cpu_c, t.gpu_c].into_iter().flatten() {
            assert!((1.0..=125.0).contains(&v), "out of range: {v}");
        }
    }

    /// Smoke: must never panic, and every reported value must be sane —
    /// covers real hardware, fanless models, and SMC-less CI VMs alike.
    #[test]
    fn fans_fail_closed() {
        for fan in fans() {
            assert!(fan.actual_rpm.is_finite() && fan.actual_rpm >= 0.0);
        }
    }
}
