//! Android devices over adb: discovery, DevTools port forward, Chrome launch,
//! emulator boot. Parsers are pure and tested; process calls are thin.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

/// `adb devices -l` output -> devices. The header and blank lines are skipped.
pub fn parse_devices(out: &str) -> Vec<AdbDevice> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts.find_map(|p| p.strip_prefix("model:").map(str::to_string));
            Some(AdbDevice { serial, state, model })
        })
        .collect()
}

/// The device a `--device` value names: `auto` = first online device, `emulator` =
/// first online `emulator-*`, else an exact serial.
pub fn choose(devices: &[AdbDevice], want: &str) -> Result<AdbDevice, String> {
    let online: Vec<&AdbDevice> = devices.iter().filter(|d| d.state == "device").collect();
    let pick = match want {
        "auto" | "" => online.first().copied(),
        "emulator" => online.iter().find(|d| d.serial.starts_with("emulator-")).copied(),
        serial => online.iter().find(|d| d.serial == serial).copied(),
    };
    pick.cloned().ok_or_else(|| match want {
        "auto" | "" => "no online Android device (adb devices is empty); plug a phone with USB debugging on, or pass --device emulator".to_string(),
        "emulator" => "no running emulator; Mini boots the `mini-qa` AVD when MINI_AVD or the default AVD exists".to_string(),
        serial => format!("device {serial} is not online (adb devices)"),
    })
}

/// Port of a loopback URL (`http://localhost:3000/x` -> 3000), else None.
pub fn loopback_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    if host == "localhost" || host == "127.0.0.1" {
        port.parse().ok()
    } else {
        None
    }
}

pub fn sdk_root() -> Option<PathBuf> {
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Ok(v) = std::env::var(var) {
            let p = PathBuf::from(v);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let p = PathBuf::from(local).join("Android").join("Sdk");
    p.exists().then_some(p)
}

pub fn adb_path() -> Result<PathBuf, String> {
    sdk_root()
        .map(|r| r.join("platform-tools").join("adb.exe"))
        .filter(|p| p.exists())
        .ok_or_else(|| "adb.exe not found: set ANDROID_SDK_ROOT or install platform-tools".to_string())
}

pub fn adb(args: &[&str]) -> Result<String, String> {
    let out = Command::new(adb_path()?).args(args).output().map_err(|e| format!("adb: {e}"))?;
    if !out.status.success() {
        return Err(format!("adb {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).replace('\r', ""))
}

pub fn devices() -> Result<Vec<AdbDevice>, String> {
    Ok(parse_devices(&adb(&["devices", "-l"])?))
}

/// True when `pm list packages <package>` output names exactly `package` (the filter also
/// matches longer names such as `com.android.chrome.canary`).
fn package_listed(out: &str, package: &str) -> bool {
    out.lines().any(|l| l.trim().strip_prefix("package:") == Some(package))
}

/// `pm list packages` exits 0 and prints nothing for a missing package. `pm path` exits 1
/// there, which `adb()` reports as an error instead of "not installed".
pub fn package_installed(serial: &str, package: &str) -> Result<bool, String> {
    Ok(package_listed(&adb(&["-s", serial, "shell", "pm", "list", "packages", package])?, package))
}

pub fn chrome_present(serial: &str) -> Result<bool, String> {
    package_installed(serial, "com.android.chrome")
}

pub fn forward_devtools(serial: &str, port: u16) -> Result<(), String> {
    adb(&["-s", serial, "forward", &format!("tcp:{port}"), "localabstract:chrome_devtools_remote"]).map(|_| ())
}

/// How often `wait_for_devtools` asks, and how long one ask may take.
const DEVTOOLS_POLL: Duration = Duration::from_millis(500);
const DEVTOOLS_ASK: Duration = Duration::from_secs(2);

/// Polls `http://127.0.0.1:<port>/json/version` until Chrome answers, or `timeout`. Chrome for
/// Android serves `chrome_devtools_remote` only once it runs a tab: right after `launch_chrome`
/// the `adb forward` port accepts a connection and drops it, so a failed ask is expected until
/// then. The first answer is the proof that `/json/list` and the page socket are ready.
pub fn wait_for_devtools(port: u16, timeout: Duration) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let start = Instant::now();
    loop {
        let last = match ureq::get(&url).config().timeout_global(Some(DEVTOOLS_ASK)).build().call() {
            Ok(_) => return Ok(()),
            Err(e) => e.to_string(),
        };
        if start.elapsed() >= timeout {
            return Err(format!("Chrome DevTools did not answer on 127.0.0.1:{port} within {:.1} s: {last}", timeout.as_secs_f32()));
        }
        std::thread::sleep(DEVTOOLS_POLL);
    }
}

pub fn reverse_port(serial: &str, port: u16) -> Result<(), String> {
    adb(&["-s", serial, "reverse", &format!("tcp:{port}"), &format!("tcp:{port}")]).map(|_| ())
}

/// Single-quote `s` for the device's `sh`. `adb shell` joins its arguments into one command
/// line: an unquoted `&` in a URL would cut it there and run the rest as a command.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn launch_chrome(serial: &str, url: &str) -> Result<(), String> {
    adb(&[
        "-s", serial, "shell", "am", "start", "-W",
        "-n", "com.android.chrome/com.google.android.apps.chrome.Main",
        "-a", "android.intent.action.VIEW", "-d", &sh_quote(url),
    ])
    .map(|_| ())
}

/// Emulator images report a generic model (`sdk_gphone64_x86_64`, `sdk_gphone_x86`, ...).
fn is_generic_emulator_model(model: &str) -> bool {
    model.starts_with("sdk_")
}

/// Device name for reports: the model on a phone (`Pixel 7a (1A2B3C4D), Android 15`);
/// on an emulator with a generic model, the AVD name (`AVD mini-qa (emulator-5554), Android 16`).
pub fn device_label(model: &str, avd_name: Option<&str>, serial: &str, release: &str) -> String {
    let model = model.trim();
    let avd = avd_name.map(str::trim).filter(|n| !n.is_empty());
    let name = match avd {
        Some(avd) if is_generic_emulator_model(model) => format!("AVD {avd}"),
        _ => model.to_string(),
    };
    format!("{name} ({serial}), Android {}", release.trim())
}

/// `device_label` built from the device's own properties.
pub fn facts(serial: &str) -> String {
    let prop = |name: &str| adb(&["-s", serial, "shell", "getprop", name]).unwrap_or_default().trim().to_string();
    let model = prop("ro.product.model");
    let avd = if is_generic_emulator_model(&model) { Some(prop("ro.boot.qemu.avd_name")) } else { None };
    device_label(&model, avd.as_deref(), serial, &prop("ro.build.version.release"))
}

/// Boots `avd` headless (MINI_EMU_WINDOW=1 shows the window) and returns its serial
/// once `sys.boot_completed` is 1.
pub fn boot_emulator(avd: &str, timeout: Duration) -> Result<String, String> {
    let emu = sdk_root().ok_or("no Android SDK")?.join("emulator").join("emulator.exe");
    let mut cmd = Command::new(emu);
    cmd.args(["-avd", avd, "-no-audio", "-no-boot-anim", "-gpu", "swiftshader_indirect"]);
    if std::env::var("MINI_EMU_WINDOW").ok().as_deref() != Some("1") {
        cmd.arg("-no-window");
    }
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008); // DETACHED_PROCESS: outlives this console
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
    cmd.spawn().map_err(|e| format!("emulator: {e}"))?;
    let start = Instant::now();
    loop {
        if let Ok(devs) = devices() {
            if let Some(d) = devs.iter().find(|d| d.serial.starts_with("emulator-") && d.state == "device") {
                let booted = adb(&["-s", &d.serial, "shell", "getprop", "sys.boot_completed"])
                    .map(|s| s.trim() == "1")
                    .unwrap_or(false);
                if booted {
                    return Ok(d.serial.clone());
                }
            }
        }
        if start.elapsed() > timeout {
            return Err(format!("emulator {avd} did not boot in {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Real `adb devices -l` output: an emulator reports its model as `sdk_gphone64_x86_64`.
    const LIST: &str = "List of devices attached\nemulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:3\n1A2B3C4D       unauthorized transport_id:2\n\n";

    #[test]
    fn parses_adb_devices_l() {
        let d = parse_devices(LIST);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], AdbDevice { serial: "emulator-5554".into(), state: "device".into(), model: Some("sdk_gphone64_x86_64".into()) });
        assert_eq!(d[1].state, "unauthorized");
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    #[test]
    fn choose_auto_emulator_and_serial() {
        let d = parse_devices(LIST);
        assert_eq!(choose(&d, "auto").unwrap().serial, "emulator-5554");
        assert_eq!(choose(&d, "emulator").unwrap().serial, "emulator-5554");
        assert!(choose(&d, "1A2B3C4D").is_err(), "unauthorized is not online");
        assert!(choose(&[], "auto").unwrap_err().contains("no online Android device"));
    }

    #[test]
    fn loopback_port_only_for_localhost() {
        assert_eq!(loopback_port("http://localhost:3000/promotions"), Some(3000));
        assert_eq!(loopback_port("http://127.0.0.1:8000/mobile.html"), Some(8000));
        assert_eq!(loopback_port("https://ohhlook.co/"), None);
    }

    #[test]
    fn device_label_uses_the_avd_name_only_for_generic_emulator_models() {
        // Emulator: the model is generic, so the AVD name is the label.
        assert_eq!(
            device_label("sdk_gphone64_x86_64", Some("mini-qa"), "emulator-5554", "16"),
            "AVD mini-qa (emulator-5554), Android 16"
        );
        // Emulator without an AVD name (property empty or missing): keep the model.
        assert_eq!(
            device_label("sdk_gphone64_x86_64", None, "emulator-5554", "16"),
            "sdk_gphone64_x86_64 (emulator-5554), Android 16"
        );
        assert_eq!(
            device_label("sdk_gphone64_x86_64", Some(" "), "emulator-5554", "16"),
            "sdk_gphone64_x86_64 (emulator-5554), Android 16"
        );
        // Phone: the model stays, an AVD name is never used.
        assert_eq!(device_label("Pixel 7a", None, "1A2B3C4D", "15"), "Pixel 7a (1A2B3C4D), Android 15");
        assert_eq!(device_label("Pixel 7a", Some("mini-qa"), "1A2B3C4D", "15"), "Pixel 7a (1A2B3C4D), Android 15");
    }

    #[test]
    fn package_listed_matches_whole_package_lines_only() {
        assert!(package_listed("package:com.android.chrome\n", "com.android.chrome"));
        assert!(package_listed("package:com.android.chrome.canary\npackage:com.android.chrome\n", "com.android.chrome"));
        // `pm list packages <filter>` matches substrings: a different package is not Chrome.
        assert!(!package_listed("package:com.android.chrome.canary\n", "com.android.chrome"));
        assert!(!package_listed("", "com.android.chrome"), "a missing package prints nothing");
    }

    /// A one-thread HTTP stub on a free loopback port. Request n (0-based) gets a 200 with
    /// `replies[n]`; `None` closes the connection without a reply, which is what `adb forward`
    /// does while Chrome has no DevTools socket yet. Returns the port and the request count.
    fn http_stub(replies: Vec<Option<&'static str>>) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::new(AtomicUsize::new(0));
        let count = served.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let n = count.fetch_add(1, Ordering::SeqCst);
                let mut head = [0u8; 2048];
                let _ = stream.read(&mut head);
                if let Some(body) = replies.get(n).copied().flatten() {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            }
        });
        (port, served)
    }

    #[test]
    fn wait_for_devtools_retries_until_chrome_answers() {
        let (port, served) = http_stub(vec![None, Some(r#"{"Browser":"Chrome/133.0.6943.137"}"#)]);
        let started = Instant::now();
        wait_for_devtools(port, Duration::from_secs(20)).expect("the second request is answered");
        assert_eq!(served.load(Ordering::SeqCst), 2, "one dropped connection, then the answer");
        assert!(started.elapsed() >= Duration::from_millis(400), "the retry waits out the poll interval");
    }

    #[test]
    fn wait_for_devtools_returns_at_once_when_chrome_is_already_up() {
        let (port, served) = http_stub(vec![Some("{}")]);
        wait_for_devtools(port, Duration::from_secs(20)).expect("answered");
        assert_eq!(served.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wait_for_devtools_gives_up_after_the_timeout_and_names_the_port() {
        // A port that was free a moment ago and has no listener now.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let started = Instant::now();
        let err = wait_for_devtools(port, Duration::from_millis(700)).unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(700), "waited the whole timeout: {:?}", started.elapsed());
        assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
        assert!(err.contains("did not answer"), "{err}");
    }

    #[test]
    fn sh_quote_keeps_a_url_one_word_for_the_device_shell() {
        assert_eq!(sh_quote("about:blank"), "'about:blank'");
        assert_eq!(sh_quote("http://localhost:3000/r?a=1&b=2"), "'http://localhost:3000/r?a=1&b=2'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
}
