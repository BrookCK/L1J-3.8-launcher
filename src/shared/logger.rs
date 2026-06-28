use std::sync::{mpsc, Mutex, OnceLock};

type SenderList = Mutex<Vec<mpsc::Sender<String>>>;
static LOG_TX: OnceLock<SenderList> = OnceLock::new();

const LOG_FILE: &str = "launcher_debug.log";
const STARTUP_DIAG_FILE: &str = "launcher_startup_timing.log";
const WRITE_LOGS_ENV: &str = "LOGIN38_WRITE_LOGS";

fn senders() -> &'static SenderList {
    LOG_TX.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn subscribe() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Ok(mut v) = senders().lock() {
        v.push(tx);
    }
    rx
}

#[allow(dead_code)]
pub fn init_channel() -> mpsc::Receiver<String> {
    subscribe()
}

pub fn log(msg: String) {
    if file_logs_enabled() {
        let file_name = if is_startup_diag_message(&msg) {
            STARTUP_DIAG_FILE
        } else {
            LOG_FILE
        };
        write_log_file(file_name, &msg);
    }

    if cfg!(feature = "verbose-log") {
        println!("{msg}");
    }

    if let Ok(mut v) = senders().lock() {
        v.retain(|tx| tx.send(msg.clone()).is_ok());
    }
}

fn file_logs_enabled() -> bool {
    file_logs_enabled_from_env(
        cfg!(feature = "verbose-log"),
        std::env::var(WRITE_LOGS_ENV).ok().as_deref(),
    )
}

fn file_logs_enabled_from_env(verbose_log: bool, env_value: Option<&str>) -> bool {
    if verbose_log {
        return true;
    }
    env_value.is_some_and(is_truthy_env)
}

fn is_truthy_env(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn is_startup_diag_message(msg: &str) -> bool {
    msg.contains("[launch-time]")
        || msg.contains("[patch-time]")
        || msg.contains("[addr-probe]")
        || msg.contains("[addr-ready]")
        || msg.contains("[inject-load]")
        || msg.contains("[inject]")
        || msg.contains("[ime-inject]")
        || msg.contains("[ime-overlay]")
        || msg.contains("[StartupHook]")
        || msg.contains("[stage2]")
        || msg.contains("[ConnectHook]")
        || msg.contains("[ImgLimit]")
        || msg.contains("[NetProxy]")
        || msg.contains("[PacketEncrypt]")
        || msg.contains("[PacketProxy]")
        || msg.contains("[spy]")
        || msg.contains("[FileHookWorker]")
        || msg.contains("[FileHook] ready wait")
        || msg.contains("[FileHook] alloc remote buffer")
        || msg.contains("[FileHook] write remote buffer")
}

fn write_log_file(file_name: &str, msg: &str) {
    let log_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(file_name)))
        .unwrap_or_else(|| std::path::PathBuf::from(file_name));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{now}] {msg}");
    }
}

// logger 同時編進 lib 與 bin crate;log_line 在 bin 到處使用,但 lib crate 內
// 各模組不一定用到 → 抑制 lib 視角的 unused 假陽性。
#[allow(unused_macros)]
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::logger::log(format!($($arg)*))
    };
}
#[allow(unused_imports)]
pub(crate) use log_line;

#[cfg(test)]
mod tests {
    #[test]
    fn file_logs_are_disabled_by_default_when_env_unset() {
        assert!(!super::file_logs_enabled_from_env(false, None));
        assert!(super::file_logs_enabled_from_env(false, Some("1")));
        assert!(super::file_logs_enabled_from_env(false, Some("true")));
        assert!(super::file_logs_enabled_from_env(true, None));
    }

    #[test]
    fn file_logs_stay_disabled_for_falsy_env() {
        assert!(!super::file_logs_enabled_from_env(false, Some("0")));
        assert!(!super::file_logs_enabled_from_env(false, Some("false")));
        assert!(!super::file_logs_enabled_from_env(false, Some("no")));
        assert!(!super::file_logs_enabled_from_env(false, Some("off")));
    }

    #[test]
    fn startup_diag_messages_are_still_classified_when_file_logging_is_enabled() {
        assert!(super::is_startup_diag_message("[stage2] all patches done"));
        assert!(super::is_startup_diag_message(
            "[launch-time] launch_game start"
        ));
        assert!(super::is_startup_diag_message(
            "[inject] transform_file=false but valid pak exists; forcing FileHook: D:\\lineage3.81C\\TW13081901.pak"
        ));
        assert!(!super::is_startup_diag_message("[drink] execute OK"));
    }
}
