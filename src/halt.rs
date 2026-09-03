use std::mem::zeroed;
use std::ptr::null_mut;
use std::thread;

use winit::event_loop::EventLoopProxy;

const SIGNALS: [i32; 2] = [libc::SIGTERM, libc::SIGINT];

pub(crate) fn on_signal(proxy: EventLoopProxy<()>) -> Result<(), String> {
    let set = set();
    unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, null_mut());
    }
    thread::Builder::new()
        .name("halt".into())
        .spawn(move || loop {
            let mut sig = 0;
            unsafe { libc::sigwait(&set, &mut sig) };
            let _ = proxy.send_event(());
        })
        .map(drop)
        .map_err(|e| e.to_string())
}

fn set() -> libc::sigset_t {
    unsafe {
        let mut set = zeroed();
        libc::sigemptyset(&mut set);
        for sig in SIGNALS {
            libc::sigaddset(&mut set, sig);
        }
        set
    }
}
