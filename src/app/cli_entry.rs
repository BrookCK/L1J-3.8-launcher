use crate::app::game_launch::launch_game;
use crate::app::launch_config::{default_game_dir, load_list_txt_default, parse_cli_args};
use anyhow::{Context, Result};

pub(crate) fn run_cli(args: &[String]) -> Result<()> {
    let (default_ip, default_port) =
        load_list_txt_default().unwrap_or_else(|| ("127.0.0.1".to_string(), 7001));
    let locked_game_dir = default_game_dir().context("cannot resolve launcher directory")?;
    let opts = parse_cli_args(args, default_ip, default_port, locked_game_dir)?;

    launch_game(
        &opts.ip,
        opts.port,
        &opts.game_dir,
        opts.no_connect,
        None,
        opts.inject_path,
        None,
        true,
        crate::app::config::WindowMode::DEFAULT,
        None,
    )
}
