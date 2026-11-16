//! The subcommands that run the engine's own code ([`zeron_cli::EngineCommand`]),
//! in a binary that links it: `zeron` itself, and `zeron-engine`, the helper
//! an interface without the engine hands them to. Logging is set up by
//! [`zeron_cli::run`] before these run.

use zeron_cli::EngineCommand;

use crate::{auth_cli, engine_config_from_env, ipc_cli, plugin_cli};

pub fn run_engine_command(command: EngineCommand) -> anyhow::Result<()> {
    match command {
        EngineCommand::Headless => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let config = engine_config_from_env();
                // A sandbox's first start: the device id its host device
                // minted for it, so its chat named it from the start.
                if config.lent_sign_in.is_some()
                    && let Ok(device_id) = std::env::var(zeron_engine::sandboxes::DEVICE_ID_ENV)
                    && !device_id.trim().is_empty()
                {
                    zeron_engine::sandboxes::take_device_id(&config.data_dir, &device_id)?;
                }
                if config.lent_sign_in.is_some()
                    && let Ok(provider) = std::env::var(zeron_engine::sandboxes::PROVIDER_ENV)
                {
                    zeron_engine::sandboxes::take_provider(&config.data_dir, &provider)?;
                }
                let engine = zeron_engine::Engine::new(config);
                engine.run().await
            })
        }
        EngineCommand::Login => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::login(engine_config_from_env()))
        }
        EngineCommand::Logout => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::logout(engine_config_from_env()))
        }
        EngineCommand::Status => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::status(engine_config_from_env()))
        }
        EngineCommand::Sync => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(ipc_cli::sync_cli(config.ipc_port, &config.data_dir))
        }
        EngineCommand::Quiesce => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(ipc_cli::quiesce_cli(config.ipc_port, &config.data_dir))
        }
        EngineCommand::ForgetDevice => {
            use zeron_engine::sandboxes::{FORGET_BUSY_EXIT, Forgot, forget_device};
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            match runtime.block_on(forget_device(&config.data_dir, config.ipc_port))? {
                Forgot::Done => {
                    println!("forgot");
                    Ok(())
                }
                Forgot::Busy(why) => {
                    eprintln!("busy: {why}");
                    std::process::exit(FORGET_BUSY_EXIT);
                }
            }
        }
        EngineCommand::Mcp => crate::run_mcp(),
        EngineCommand::Plugin(command) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(plugin_cli::run(command, config.ipc_port, &config.data_dir))
        }
        EngineCommand::Template(command) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(plugin_cli::run_template(
                command,
                config.ipc_port,
                &config.data_dir,
            ))
        }
        EngineCommand::Engine => crate::background::run(),
    }
}
