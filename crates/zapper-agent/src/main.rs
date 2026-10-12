//! zapper-agent: PID 1 of a zapper microVM.
//!
//! It mounts the basics, fetches the session spec from the host over vsock,
//! prepares the guest, then serves terminals whose every exec is checked by
//! the host and whose process exits are reported to it.
//!
//! Nothing here is trusted by the host: the agent holds no secrets, and the
//! host enforces network, secret and disk policy on its own side.

mod acct;
mod control;
mod forward;
mod init;
mod shell;
mod sys;

fn main() {
    if std::process::id() == 1 {
        // A panic in PID 1 would panic the kernel; power off instead.
        std::panic::set_hook(Box::new(|info| {
            eprintln!("zapper-agent: {info}");
            sys::power_off();
        }));
        init::run();
    }
    match std::env::args().nth(1).as_deref() {
        Some("--version") => println!("zapper-agent {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("zapper-agent runs as PID 1 of a zapper microVM");
            std::process::exit(2);
        }
    }
}
