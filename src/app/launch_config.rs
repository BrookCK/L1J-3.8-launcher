use crate::logger::log_line;
use anyhow::{bail, Context, Result};
use std::net::Ipv4Addr;

pub(crate) const DEFAULT_STAGE2_DELAY_MS: u64 = 0;
pub(crate) const DEFAULT_STAGE2_REMAINING_PATCH_DELAY_MS: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectTarget {
    pub(crate) ip: String,
    pub(crate) port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliLaunchOptions {
    pub(crate) ip: String,
    pub(crate) port: u16,
    pub(crate) game_dir: String,
    pub(crate) no_connect: bool,
    pub(crate) inject_path: Option<String>,
}

pub(crate) fn should_attach_console(args: &[String]) -> bool {
    let stage2_mode = args.get(1).map(|s| s == "--stage2").unwrap_or(false);
    cfg!(feature = "verbose-log") && args.len() > 1 && !stage2_mode
}

pub(crate) fn parse_cli_args(
    args: &[String],
    default_ip: String,
    default_port: u16,
    locked_game_dir: String,
) -> Result<CliLaunchOptions> {
    let mut ip = default_ip;
    let mut port = default_port;
    let mut no_connect = false;
    let mut inject_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "usage: {} [IP PORT] [--inject FILE] [--no-connect]",
                    args[0]
                );
                println!("  --inject FILE  load .pak or .txt inject file");
                println!("  --no-connect   skip connect hook");
                println!("  no args        open GUI");
                std::process::exit(0);
            }
            "--no-connect" => no_connect = true,
            "--no-smooth-run-hook" => {
                std::env::set_var("LOGIN38_DISABLE_SMOOTH_RUN_HOOK", "1");
            }
            "--inject" => {
                i += 1;
                if i >= args.len() {
                    bail!("--inject requires a file path");
                }
                inject_path = Some(args[i].clone());
            }
            value => {
                if i + 1 < args.len() && args[i + 1].parse::<u16>().is_ok() {
                    let _: Ipv4Addr = value.parse().context("CLI IP parse failed")?;
                    ip = value.to_string();
                    port = args[i + 1]
                        .parse::<u16>()
                        .context("CLI port parse failed")?;
                    i += 1;
                } else {
                    bail!("unknown argument: {value}");
                }
            }
        }
        i += 1;
    }

    Ok(CliLaunchOptions {
        ip,
        port,
        game_dir: locked_game_dir,
        no_connect,
        inject_path,
    })
}

pub(crate) fn default_game_dir() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    default_game_dir_from_exe_path(&exe)
}

pub(crate) fn default_game_dir_from_exe_path(exe: &std::path::Path) -> Option<String> {
    exe.parent().map(|dir| dir.to_string_lossy().into_owned())
}

pub(crate) fn load_list_txt_default() -> Option<(String, u16)> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let list_path = exe_dir.join("list.txt");
    if !list_path.exists() {
        return None;
    }

    let raw = launcher::legacy_text::read_text_file(&list_path).ok()?;
    let plain = launcher::server_list::decrypt_config_text(&raw).unwrap_or(raw);
    let servers = launcher::server_list::parse_list_txt(&plain).ok()?;
    let active = servers.into_iter().find(|s| s.used)?;
    let port = u16::try_from(active.port).ok()?;
    log_line!(
        "[config] list.txt active server: {} {}:{}",
        active.name,
        active.ip,
        port
    );
    Some((active.ip, port))
}

pub(crate) fn load_aux_config() -> launcher::server_list::AuxConfig {
    let result = (|| -> Option<launcher::server_list::AuxConfig> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        for name in ["config.ini", "list.txt"] {
            let path = exe_dir.join(name);
            if !path.exists() {
                continue;
            }
            let raw = launcher::legacy_text::read_text_file(&path).ok()?;
            let plain = launcher::server_list::decrypt_config_text(&raw).unwrap_or(raw);
            if let Ok(parsed) = launcher::server_list::parse_list_file(&plain) {
                return Some(parsed.aux);
            }
        }
        None
    })();
    let aux = result.unwrap_or_default();
    crate::legacy_text::set_text_encoding_mode(
        crate::legacy_text::TextEncodingMode::from_config_value(
            aux.text_encoding.as_config_value(),
        ),
    );
    aux
}

pub(crate) fn enabled_by_default_env_flag(value: Option<&std::ffi::OsStr>) -> bool {
    value
        .and_then(|v| v.to_str())
        .map(|v| {
            let v = v.trim();
            !(v == "0"
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("no")
                || v.eq_ignore_ascii_case("off"))
        })
        .unwrap_or(true)
}

pub(crate) fn opt_in_env_flag_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value
        .and_then(|v| v.to_str())
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

pub(crate) fn game_ip_arg_requested(value: Option<&std::ffi::OsStr>) -> bool {
    opt_in_env_flag_requested(value)
}

pub(crate) fn game_ip_arg_enabled() -> bool {
    let value = std::env::var_os("LOGIN38_GAME_IP_ARG");
    game_ip_arg_requested(value.as_deref())
}

pub(crate) fn connect_hook_requested(no_connect: bool, value: Option<&std::ffi::OsStr>) -> bool {
    !no_connect && enabled_by_default_env_flag(value)
}

pub(crate) fn connect_hook_enabled(no_connect: bool) -> bool {
    let value = std::env::var_os("LOGIN38_CONNECT_HOOK");
    connect_hook_requested(no_connect, value.as_deref())
}

pub(crate) fn keep_launcher_alive_after_stage2_requested(value: Option<&std::ffi::OsStr>) -> bool {
    opt_in_env_flag_requested(value)
}

pub(crate) fn keep_launcher_alive_after_stage2_enabled() -> bool {
    let value = std::env::var_os("LOGIN38_KEEP_LAUNCHER_ALIVE");
    keep_launcher_alive_after_stage2_requested(value.as_deref())
}

pub(crate) fn stage2_pre_visible_attach_requested(value: Option<&std::ffi::OsStr>) -> bool {
    opt_in_env_flag_requested(value)
}

pub(crate) fn stage2_pre_visible_attach_enabled() -> bool {
    let value = std::env::var_os("LOGIN38_STAGE2_ATTACH_BEFORE_WINDOW")
        .or_else(|| std::env::var_os("LOGIN38_STAGE2_PRE_VISIBLE_ATTACH"));
    stage2_pre_visible_attach_requested(value.as_deref())
}

pub(crate) fn stage2_delay_ms() -> u64 {
    std::env::var("LOGIN38_STAGE2_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|ms| *ms <= 120_000)
        .unwrap_or(DEFAULT_STAGE2_DELAY_MS)
}

pub(crate) fn stage2_remaining_patch_delay_ms() -> u64 {
    std::env::var("LOGIN38_STAGE2_REMAINING_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|ms| *ms <= 120_000)
        .unwrap_or(DEFAULT_STAGE2_REMAINING_PATCH_DELAY_MS)
}

pub(crate) fn marker_file_present(name: &str) -> bool {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join(name).exists();
        }
    }
    false
}

pub(crate) fn env_truthy(var: &str) -> bool {
    let Some(raw) = std::env::var_os(var) else {
        return false;
    };
    matches!(
        raw.to_string_lossy().trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(crate) fn smooth_run_hook_disabled_by_env() -> bool {
    env_truthy("LOGIN38_DISABLE_SMOOTH_RUN_HOOK")
        || marker_file_present("disable_smooth_run_hook.flag")
}

pub(crate) fn file_hook_disabled_by_env() -> bool {
    env_truthy("LOGIN38_DISABLE_FILE_HOOK") || marker_file_present("disable_file_hook.flag")
}

pub(crate) fn send_packet_spy_enabled_by_env() -> bool {
    env_truthy("LOGIN38_ENABLE_SEND_PACKET_SPY")
        || marker_file_present("enable_send_packet_spy.flag")
}

pub(crate) fn force_simplified_text_locale_enabled_by_env() -> bool {
    env_truthy("LOGIN38_FORCE_SIMPLIFIED_TEXT_LOCALE")
        || marker_file_present("force_simplified_text_locale.flag")
}

pub(crate) fn img_hover_disabled_by_env() -> bool {
    env_truthy("LOGIN38_DISABLE_IMG_HOVER") || marker_file_present("disable_img_hover.flag")
}

pub(crate) fn img_limit_disabled_by_env() -> bool {
    env_truthy("LOGIN38_DISABLE_IMG_LIMIT") || marker_file_present("disable_img_limit.flag")
}

pub(crate) fn hp_mp_limit_disabled_by_env() -> bool {
    env_truthy("LOGIN38_DISABLE_HP_MP_LIMIT") || marker_file_present("disable_hp_mp_limit.flag")
}

pub(crate) fn equip_ui_disabled_by_env() -> bool {
    env_truthy("LOGIN38_DISABLE_EQUIP_UI") || marker_file_present("disable_equip_ui.flag")
}

pub(crate) fn create_game_suspended_requested(pre_resume_startup_hook: bool) -> bool {
    pre_resume_startup_hook
}

pub(crate) fn packet_encrypt_requires_startup_hook(_enabled: bool) -> bool {
    false
}

pub(crate) fn connect_target_for_launch(
    real_ip: &str,
    real_port: u16,
    proxy: Option<(&str, u16)>,
) -> ConnectTarget {
    match proxy {
        Some((ip, port)) => ConnectTarget {
            ip: ip.to_string(),
            port,
        },
        None => ConnectTarget {
            ip: real_ip.to_string(),
            port: real_port,
        },
    }
}

pub(crate) fn ipv4_decimal_arg(ip: &str) -> Option<String> {
    let addr: Ipv4Addr = ip.parse().ok()?;
    Some(u32::from(addr).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_arg_uses_decimal_form_expected_by_bin() {
        assert_eq!(ipv4_decimal_arg("127.0.0.1").as_deref(), Some("2130706433"));
    }

    #[test]
    fn game_ip_arg_is_opt_in() {
        assert!(!game_ip_arg_requested(None));
        assert!(!game_ip_arg_requested(Some("0".as_ref())));
        assert!(!game_ip_arg_requested(Some("false".as_ref())));
        assert!(game_ip_arg_requested(Some("1".as_ref())));
    }

    #[test]
    fn direct_bin_mode_does_not_create_suspended_process() {
        assert!(!create_game_suspended_requested(false));
        assert!(create_game_suspended_requested(true));
    }

    #[test]
    fn packet_encrypt_does_not_require_startup_hook_dll() {
        assert!(!packet_encrypt_requires_startup_hook(true));
        assert!(!create_game_suspended_requested(
            packet_encrypt_requires_startup_hook(true)
        ));
    }

    #[test]
    fn packet_encrypt_routes_connect_to_local_proxy_endpoint() {
        let target = connect_target_for_launch("203.0.113.10", 7000, Some(("127.0.0.1", 49152)));

        assert_eq!(target.ip, "127.0.0.1");
        assert_eq!(target.port, 49152);
    }

    #[test]
    fn plain_launch_routes_connect_to_real_server() {
        let target = connect_target_for_launch("203.0.113.10", 7000, None);

        assert_eq!(target.ip, "203.0.113.10");
        assert_eq!(target.port, 7000);
    }

    #[test]
    fn release_build_does_not_attach_console_for_cli_logs() {
        let args = vec![
            "launcher.exe".to_string(),
            "127.0.0.1".to_string(),
            "7001".to_string(),
        ];

        assert_eq!(should_attach_console(&args), cfg!(feature = "verbose-log"));
    }

    #[test]
    fn stage2_never_attaches_console() {
        let args = vec!["launcher.exe".to_string(), "--stage2".to_string()];

        assert!(!should_attach_console(&args));
    }

    #[test]
    fn default_game_dir_is_launcher_exe_parent_not_hardcoded_path() {
        let path = std::path::Path::new(r"D:\locked-client\launcher.exe");

        assert_eq!(
            default_game_dir_from_exe_path(path).as_deref(),
            Some(r"D:\locked-client")
        );
    }

    #[test]
    fn cli_ip_port_keeps_launcher_exe_parent_as_game_dir() {
        let args = vec![
            "launcher.exe".to_string(),
            "192.168.1.10".to_string(),
            "7000".to_string(),
        ];

        let parsed = parse_cli_args(
            &args,
            "127.0.0.1".to_string(),
            7001,
            r"D:\locked-client".to_string(),
        )
        .unwrap();

        assert_eq!(parsed.ip, "192.168.1.10");
        assert_eq!(parsed.port, 7000);
        assert_eq!(parsed.game_dir, r"D:\locked-client");
    }

    #[test]
    fn cli_rejects_positional_game_dir_override() {
        let args = vec!["launcher.exe".to_string(), r"D:\other-client".to_string()];

        let err = parse_cli_args(
            &args,
            "127.0.0.1".to_string(),
            7001,
            r"D:\locked-client".to_string(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("unknown argument"));
    }

    #[test]
    fn cli_rejects_path_even_when_followed_by_port() {
        let args = vec![
            "launcher.exe".to_string(),
            r"D:\other-client".to_string(),
            "7001".to_string(),
        ];

        let err = parse_cli_args(
            &args,
            "127.0.0.1".to_string(),
            7001,
            r"D:\locked-client".to_string(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("CLI IP parse failed"));
    }

    #[test]
    fn connect_hook_is_enabled_by_default() {
        assert!(connect_hook_requested(false, None));
        assert!(!connect_hook_requested(false, Some("0".as_ref())));
        assert!(!connect_hook_requested(true, Some("1".as_ref())));
        assert!(connect_hook_requested(false, Some("1".as_ref())));
    }

    #[test]
    fn stage2_pre_visible_attach_is_opt_in() {
        assert!(!stage2_pre_visible_attach_requested(None));
        assert!(!stage2_pre_visible_attach_requested(Some("0".as_ref())));
        assert!(stage2_pre_visible_attach_requested(Some("1".as_ref())));
    }

    #[test]
    fn launcher_parent_exits_after_stage2_by_default() {
        assert!(!keep_launcher_alive_after_stage2_requested(None));
        assert!(!keep_launcher_alive_after_stage2_requested(Some(
            "0".as_ref()
        )));
        assert!(keep_launcher_alive_after_stage2_requested(Some(
            "1".as_ref()
        )));
    }

    #[test]
    fn stage2_attaches_immediately_by_default() {
        assert_eq!(DEFAULT_STAGE2_DELAY_MS, 0);
    }
}
