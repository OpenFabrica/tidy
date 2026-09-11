// Manual probe for the fan daemon: `fanctl start|status|set <rpm>|auto|stop`.
// `start` launches `target/debug/mole-helper` next to this example through
// the admin prompt, exactly like the app does; the daemon is tied to THIS
// process' pid, so it restores auto and exits when fanctl exits — pass
// `--hold <secs>` after `start` to keep it alive for a manual session.
use mole_ops::fancontrol;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = match args.get(1).map(String::as_str) {
        Some("start") => {
            let helper: PathBuf = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("mole-helper");
            let r = fancontrol::start(&helper).map(|_| "started".to_string());
            if let (Ok(_), Some(secs)) = (&r, args.get(3).and_then(|s| s.parse::<u64>().ok())) {
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
            r
        }
        Some("status") => Ok(format!("{:?}", fancontrol::status())),
        Some("set") => args
            .get(2)
            .and_then(|s| s.parse::<f32>().ok())
            .ok_or_else(|| "usage: fanctl set <rpm>".to_string())
            .and_then(|rpm| fancontrol::set_rpm(rpm).map(|a| format!("applied {a}"))),
        Some("auto") => fancontrol::set_auto().map(|_| "auto".to_string()),
        Some("stop") => fancontrol::stop().map(|_| "stopped".to_string()),
        _ => Err("usage: fanctl start [--hold <secs>] | status | set <rpm> | auto | stop".into()),
    };
    match out {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
