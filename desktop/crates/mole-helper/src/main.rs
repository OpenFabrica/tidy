// Helper binary entry point.
//
//   mole-helper fan-daemon --gui-pid <pid> --uid <uid>
//       Root-only fan daemon (see fan.rs). Launched by the GUI through the
//       admin prompt; exits and restores automatic fans when the GUI is gone.
//
// The XPC service loop (SMAppService registration, audit-token identity,
// per-request vetting through lib.rs) lands with the signing phase of the
// release tooling; any other invocation does nothing privileged and says so.

mod fan;

/// Parse `--name <value>` from the argument list as an integer.
fn flag<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1)?.parse().ok()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("fan-daemon") {
        let (Some(gui_pid), Some(uid)) =
            (flag::<i32>(&args, "--gui-pid"), flag::<u32>(&args, "--uid"))
        else {
            eprintln!("usage: mole-helper fan-daemon --gui-pid <pid> --uid <uid>");
            std::process::exit(2);
        };
        if let Err(e) = fan::run(gui_pid, uid) {
            eprintln!("mole-helper fan-daemon: {e}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!(
        "mole-helper: this binary only serves XPC requests when registered via \
         SMAppService inside the signed Mole Desktop bundle; direct invocation does nothing."
    );
    std::process::exit(1);
}
