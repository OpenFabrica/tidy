// GPU utilisation straight from the IOAccelerator registry entry's
// "PerformanceStatistics" dictionary (the numbers Activity Monitor's GPU
// History uses) — one IOKit property read, no ioreg subprocess. Apple Silicon
// exposes one AGX accelerator; Intel Macs may list several (iGPU + dGPU), in
// which case the busiest one is reported.

use super::ffi::{dict_int, IoObject};

/// One GPU observation.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct GpuSample {
    pub name: String,
    pub core_count: Option<u32>,
    pub usage: Option<f32>,
    pub renderer: Option<f32>,
    pub tiler: Option<f32>,
    pub memory_used_bytes: Option<u64>,
}

/// Percent value from the statistics dictionary, clamped to 0–100.
fn percent(value: Option<i64>) -> Option<f32> {
    value.map(|v| v.clamp(0, 100) as f32)
}

/// Read every accelerator; the busiest one with statistics wins.
pub(crate) fn read() -> Option<GpuSample> {
    IoObject::services(c"IOAccelerator")
        .into_iter()
        .filter_map(|acc| {
            let stats = acc.dict("PerformanceStatistics")?;
            let usage = percent(dict_int(&stats, "Device Utilization %"));
            let renderer = percent(dict_int(&stats, "Renderer Utilization %"));
            let tiler = percent(dict_int(&stats, "Tiler Utilization %"));
            let memory_used_bytes = dict_int(&stats, "In use system memory")
                .or_else(|| dict_int(&stats, "vramUsedBytes"))
                .and_then(|v| u64::try_from(v).ok());
            // Apple Silicon: "model" on the accelerator itself; Intel/AMD:
            // the PCI parent carries "model" as bytes.
            let name = acc
                .string("model")
                .or_else(|| {
                    let parent = acc.parent()?;
                    parent.string("model").or_else(|| {
                        let raw = parent.data("model")?;
                        let text = String::from_utf8_lossy(&raw);
                        Some(text.trim_end_matches('\0').trim().to_string())
                    })
                })
                .unwrap_or_default();
            let core_count = acc
                .int("gpu-core-count")
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0);
            Some(GpuSample {
                name,
                core_count,
                usage,
                renderer,
                tiler,
                memory_used_bytes,
            })
        })
        .max_by(|a, b| a.usage.unwrap_or(-1.0).total_cmp(&b.usage.unwrap_or(-1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_clamps() {
        assert_eq!(percent(Some(150)), Some(100.0));
        assert_eq!(percent(Some(-3)), Some(0.0));
        assert_eq!(percent(None), None);
    }

    /// Smoke: never panics; any reported usage is a sane percentage.
    #[test]
    fn live_read_is_sane() {
        if let Some(s) = read() {
            for v in [s.usage, s.renderer, s.tiler].into_iter().flatten() {
                assert!((0.0..=100.0).contains(&v));
            }
        }
    }
}
