fn main() -> Result<(), winit::error::EventLoopError> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("lightherder=info,wgpu=warn"),
    )
    .init();
    print!("{}", lightherder::keys::help());
    lightherder::app::run()
}
