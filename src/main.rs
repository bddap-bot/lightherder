use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("lightherder=info,wgpu=warn"),
    )
    .init();
    let arg = std::env::args().nth(1).unwrap_or_else(|| "single".into());
    let params = match lightherder::config::load(&arg) {
        Ok(params) => params,
        Err(why) => {
            eprintln!("lightherder: {why}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", lightherder::keys::help());
    if let Err(why) = lightherder::app::run(params) {
        eprintln!("lightherder: {why}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
