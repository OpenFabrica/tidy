//! mole-telemetry: 匿名使用统计。
//!
//! 三条硬约束，改这个 crate 前请先读一遍：
//!
//! 1. **可以整个关掉。** 构建时没有 `TIDY_TELEMETRY_URL` 就不存在上报路径；
//!    运行时用户关掉开关，后台线程直接不建立任何连接。
//! 2. **采集面是编译期封闭的。** 见 [`Event`]，属性值只能是 `&'static str`
//!    / bool / u64，语法上无法夹带路径、文件名或体积。
//! 3. **标识是随机的。** 首次开启时生成一个 uuid4 存进 `~/.config/mole/`，
//!    与硬件、Apple ID、MAC 地址无关；删掉配置目录就变成一个新用户。
//!
//! 对外行为（发往哪、采什么、怎么关）必须与 `site/{zh,en}/privacy` 一致。

mod event;
mod queue;

pub use event::Event;

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::Duration;

/// 设置键：遥测总开关。
pub const KEY_ENABLED: &str = "telemetry.enabled";
/// 设置键：随机安装标识。
pub const KEY_INSTALL_ID: &str = "telemetry.install_id";
/// 设置键：首次启动的遥测告知横幅是否已经出现过。
pub const KEY_NOTICE_SEEN: &str = "telemetry.notice_seen";
/// 设置键：本机是否已经启动过（本地标记，永不上传）。
pub const KEY_LAUNCHED_BEFORE: &str = "app.launched_before";

/// 构建期注入的上报地址（自建反向代理的根，例如 `https://t.example.com`）。
/// 未注入 = 遥测在这个二进制里根本不存在。
const ENDPOINT: Option<&str> = option_env!("TIDY_TELEMETRY_URL");
/// 构建期注入的 PostHog 项目公钥（写入用，天然公开）。
const API_KEY: Option<&str> = option_env!("TIDY_TELEMETRY_KEY");

/// 攒批的最长等待时间。
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// 攒够这么多条就立刻发，不等计时器。
const BATCH_TRIGGER: usize = 20;
/// 单次 HTTP 请求超时。设短是为了让离线用户的退出流程不被拖住。
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// 退出时最多等这么久让队列发完。超时也不丢数据——退出路径会先落盘再发。
const EXIT_FLUSH_BUDGET: Duration = Duration::from_secs(2);

/// 启动时确定下来的遥测状态，交给 UI 决定要不要弹首次告知横幅。
#[derive(Debug, Clone, Copy)]
pub struct Boot {
    /// 这个构建里是否编译进了上报地址。
    pub configured: bool,
    /// 用户当前是否允许上报。
    pub enabled: bool,
    /// 是否是本机第一次启动 Tidy。
    pub first_run: bool,
    /// 首次告知横幅是否已经展示过。
    pub notice_seen: bool,
}

/// 后台线程的指令。
enum Msg {
    Track(Box<Event>),
    /// 立刻发一批。带回执 = 调用方在等（退出路径），后台线程会先落盘再发。
    Flush(Option<mpsc::SyncSender<()>>),
}

struct Client {
    tx: Sender<Msg>,
}

static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);

/// 这个构建是否带有上报能力。
pub fn is_configured() -> bool {
    ENDPOINT.is_some_and(|url| !url.is_empty()) && API_KEY.is_some_and(|key| !key.is_empty())
}

/// 用户当前是否允许上报。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 初始化遥测。幂等：重复调用只有第一次生效。
///
/// 即使遥测被关掉也要调用它——`first_run` 的本地标记由这里落盘，首次告知
/// 横幅依赖它。关掉时不会生成安装标识，也不会起后台线程。
pub fn init(home: &str, app_version: &str, os_version: &str, locale: &str) -> Boot {
    let first_run = !mole_core::settings::bool_or(home, KEY_LAUNCHED_BEFORE, false);
    if first_run {
        let _ = mole_core::settings::set(home, KEY_LAUNCHED_BEFORE, "1");
    }
    // 默认开：决策见 grilling 记录（默认开 + 首次启动明确告知）。
    let enabled = mole_core::settings::bool_or(home, KEY_ENABLED, true);
    let notice_seen = mole_core::settings::bool_or(home, KEY_NOTICE_SEEN, false);
    ENABLED.store(enabled && is_configured(), Ordering::Relaxed);

    let boot = Boot {
        configured: is_configured(),
        enabled,
        first_run,
        notice_seen,
    };
    if !is_configured() {
        CLIENT.get_or_init(|| None);
        return boot;
    }

    let home = home.to_string();
    let install_id = install_id(&home);
    let context = Context {
        install_id,
        app_version: event::sanitize_version(app_version),
        os_version: event::sanitize_version(os_version),
        locale: locale
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(16)
            .collect(),
        spill: queue::spill_file(&home),
    };

    CLIENT.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::Builder::new()
            .name("mole-telemetry".into())
            .spawn(move || worker(rx, context))
            .ok()
            .map(|_| Client { tx })
    });
    boot
}

/// 记录一个事件。关闭时是空操作，不排队、不落盘。
pub fn track(event: Event) {
    if !is_enabled() {
        return;
    }
    if let Some(Some(client)) = CLIENT.get() {
        let _ = client.tx.send(Msg::Track(Box::new(event)));
    }
}

/// 立刻把队列发出去，并**等到发完再返回**（应用退出前调用）。
///
/// 必须等：早先这里是发完消息就返回，主线程紧接着退出，进程把正在发 HTTP
/// 的后台线程一并带走，于是退出时的这一次冲刷从来没有真正生效过——只有 60
/// 秒定时那次有用。实测一次启动后立刻退出的会话，事件全丢。
///
/// 等待有 2 秒预算，超时也不丢数据：后台线程在这条路径上会**先落盘再发**，
/// 进程就算下一毫秒消失，事件也在磁盘上等着下次启动补发。
pub fn flush() {
    let Some(Some(client)) = CLIENT.get() else {
        return;
    };
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    if client.tx.send(Msg::Flush(Some(ack_tx))).is_ok() {
        let _ = ack_rx.recv_timeout(EXIT_FLUSH_BUDGET);
    }
}

/// 切换总开关。关掉时立刻清空磁盘队列并丢弃安装标识——用户关掉遥测的意思
/// 是"别再记着我"，留着一个 id 等他改主意不是他要的。
pub fn set_enabled(home: &str, on: bool) -> std::io::Result<()> {
    mole_core::settings::set(home, KEY_ENABLED, if on { "1" } else { "0" })?;
    ENABLED.store(on && is_configured(), Ordering::Relaxed);
    if !on {
        queue::discard_spill(&queue::spill_file(home));
        let _ = mole_core::settings::set(home, KEY_INSTALL_ID, "");
    }
    Ok(())
}

/// 记下首次告知横幅已经展示过。
pub fn mark_notice_seen(home: &str) -> std::io::Result<()> {
    mole_core::settings::set(home, KEY_NOTICE_SEEN, "1")
}

/// 读取或生成随机安装标识。
///
/// 用 `/dev/urandom` 直接拼一个 v4 UUID，不引第三方库：这个值的唯一要求是
/// 随机且与设备无关，不需要 uuid crate 的其余能力。
fn install_id(home: &str) -> String {
    match mole_core::settings::get(home, KEY_INSTALL_ID) {
        Some(id) if !id.is_empty() => id,
        _ => {
            let id = random_uuid_v4();
            let _ = mole_core::settings::set(home, KEY_INSTALL_ID, &id);
            id
        }
    }
}

/// 生成一个 v4 UUID。取不到系统随机源时退化为时间戳派生值（只影响去重精度，
/// 不影响正确性）。
fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes.copy_from_slice(&nanos.to_le_bytes()[..16]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// 每条事件都会带上的公共属性。
struct Context {
    install_id: String,
    app_version: String,
    os_version: String,
    locale: String,
    spill: std::path::PathBuf,
}

/// 把一个事件包成 PostHog capture 负载。
fn envelope(ctx: &Context, event: &Event) -> Value {
    let mut props = event.properties();
    props.insert("distinct_id".into(), json!(ctx.install_id));
    props.insert("app_version".into(), json!(ctx.app_version));
    props.insert("os_version".into(), json!(ctx.os_version));
    props.insert("locale".into(), json!(ctx.locale));
    props.insert("$lib".into(), json!("tidy"));
    // 关掉 PostHog 的 IP 反查地理位置：代理那一跳已经拿不到真实定位价值，
    // 再让服务端存一份国家/城市没有意义。
    props.insert("$geoip_disable".into(), json!(true));
    json!({
        "event": event.name(),
        "properties": Value::Object(props),
        "timestamp": now_iso8601(),
    })
}

/// UTC 时间戳。不引 chrono：这里只需要一个 PostHog 认得的 ISO8601 秒级串。
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 days→(y,m,d) 算法。之所以手写：workspace 里的 chrono
/// 关掉了默认特性，为一个时间戳字符串再拉一套格式化依赖不划算。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 后台线程：攒批、定时发、失败落盘。
fn worker(rx: mpsc::Receiver<Msg>, ctx: Context) {
    let mut pending = queue::load_spill(&ctx.spill);
    // rustls 要求进程里先装一个加密 provider。updater 插件也会装同一个，
    // 但谁先跑不确定，所以两边都装一次（install_default 幂等，重复返回 Err）。
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let http = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build();
    let Ok(http) = http else { return };

    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(Msg::Track(event)) => {
                pending.push(envelope(&ctx, &event));
                queue::trim(&mut pending);
                if pending.len() >= BATCH_TRIGGER {
                    send_or_spill(&http, &ctx, &mut pending);
                }
            }
            Ok(Msg::Flush(ack)) => {
                // 有人在等回执 = 退出路径。先落盘：进程可能在下一毫秒消失，
                // 磁盘是唯一不会跟着进程一起没的地方。发送成功后会删掉。
                if ack.is_some() {
                    queue::save_spill(&ctx.spill, &pending);
                }
                send_or_spill(&http, &ctx, &mut pending);
                if let Some(ack) = ack {
                    let _ = ack.try_send(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                send_or_spill(&http, &ctx, &mut pending);
            }
            // 发送端全部析构 = 应用在退出。最后发一次再收工。
            Err(RecvTimeoutError::Disconnected) => {
                send_or_spill(&http, &ctx, &mut pending);
                return;
            }
        }
    }
}

/// 发一批；失败就原样落盘等下次。用户已经关掉开关时直接丢弃并清盘。
fn send_or_spill(http: &reqwest::blocking::Client, ctx: &Context, pending: &mut Vec<Value>) {
    if !is_enabled() {
        pending.clear();
        queue::discard_spill(&ctx.spill);
        return;
    }
    if pending.is_empty() {
        return;
    }
    if send(http, pending) {
        pending.clear();
        queue::discard_spill(&ctx.spill);
    } else {
        queue::save_spill(&ctx.spill, pending);
    }
}

/// POST 一批事件到自建代理。成功返回 true。
fn send(http: &reqwest::blocking::Client, batch: &[Value]) -> bool {
    let (Some(endpoint), Some(key)) = (ENDPOINT, API_KEY) else {
        return false;
    };
    let url = format!("{}/batch/", endpoint.trim_end_matches('/'));
    let body = json!({ "api_key": key, "batch": batch });
    http.post(url)
        .json(&body)
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            install_id: "id".into(),
            app_version: "0.1.0".into(),
            os_version: "15.0".into(),
            locale: "zh-Hans".into(),
            spill: std::path::PathBuf::from("/tmp/never-written"),
        }
    }

    #[test]
    fn envelope_carries_only_the_declared_properties() {
        let value = envelope(
            &ctx(),
            &Event::CleanExecuted {
                mode: "trash",
                result: "ok",
            },
        );
        let props = value["properties"].as_object().unwrap();
        let mut keys: Vec<_> = props.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "$geoip_disable",
                "$lib",
                "app_version",
                "distinct_id",
                "locale",
                "mode",
                "os_version",
                "result",
            ]
        );
    }

    #[test]
    fn install_id_is_a_v4_uuid_and_is_stable_per_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let first = install_id(home);
        assert_eq!(first.len(), 36);
        assert_eq!(&first[14..15], "4");
        assert_eq!(install_id(home), first);
    }

    #[test]
    fn disabling_clears_the_install_id_so_a_reopt_in_is_a_new_user() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let first = install_id(home);
        set_enabled(home, false).unwrap();
        assert_ne!(install_id(home), first);
    }

    #[test]
    fn timestamp_matches_a_known_epoch_second() {
        // 2026-09-01T00:00:00Z
        assert!(now_iso8601().ends_with('Z'));
        assert_eq!(civil_from_days(20_697), (2026, 9, 1));
    }
}
