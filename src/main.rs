fn main() -> Result<(), winit::error::EventLoopError> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("lightherder=info,wgpu=warn"),
    )
    .init();
    println!("{}", lightherder::app::KEY_HELP);
    lightherder::app::run()
}
