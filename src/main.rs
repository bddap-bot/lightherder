use std::process::ExitCode;

use lightherder::cli::{Cli, Mode};

fn main() -> ExitCode {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("lightherder=info,wgpu=warn"),
    )
    .init();
    match play() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("lightherder: {why}");
            ExitCode::FAILURE
        }
    }
}

fn play() -> Result<(), Box<dyn std::error::Error>> {
    let cli: Cli = lightherder::cli::parse(std::env::args().skip(1))?;
    if cli.mode == Mode::Usage {
        print!("{}", lightherder::cli::usage());
        return Ok(());
    }
    let params = cli.instrument();
    match cli.mode {
        Mode::Bench => Ok(pollster::block_on(lightherder::bench::run(
            &params,
            cli.resolution,
        ))?),
        _ => pollster::block_on(lightherder::app::run(params, &cli)),
    }
}
