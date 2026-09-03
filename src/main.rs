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
    let params = lightherder::config::instrument();
    // The controls are the one thing worth printing without a GPU or a
    // display: it is the card a performer reads while setting up.
    if cli.mode == Mode::Cheatsheet {
        print!("{}", lightherder::cheatsheet(&params)?);
        return Ok(());
    }
    match cli.mode {
        Mode::Bench => Ok(pollster::block_on(lightherder::bench::run(
            &params,
            cli.resolution,
        ))?),
        // The instrument prints its own controls once it has the map it will
        // play, so there is one read of that file rather than two.
        _ => pollster::block_on(lightherder::app::run(params, &cli)),
    }
}
