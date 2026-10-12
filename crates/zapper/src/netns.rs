//! `zapper __netns`: runs a session's VMM in its own user and network
//! namespace, whose only way out is the daemon.
//!
//! Inside the namespace there is loopback and `tap0`, the guest's NIC, and
//! nothing else: no route, no other interface. nftables there redirects every
//! TCP connection from the guest to a local listener and every DNS query to
//! another; everything else from the guest is dropped. Both listeners hand
//! their traffic to the daemon over unix sockets in the session directory,
//! where policy applies. Because the capture is outside the VM, root in the
//! guest cannot get around it. No host privileges are needed: an unprivileged
//! user namespace owns the network namespace.
//!
//! Runs before any other thread exists, as `unshare(CLONE_NEWUSER)` requires.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 1);
pub const GUEST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
pub const GUEST_MAC: &str = "06:00:0a:00:02:02";
pub const TAP: &str = "tap0";
const TCP_PORT: u16 = 15001;
const DNS_PORT: u16 = 15053;
/// Daemon socket in the session directory.
pub const SOCKET: &str = "nic.sock";

/// Kernel command line that configures the guest's `eth0`.
pub fn boot_args() -> String {
    format!("ip={GUEST}::{GATEWAY}:255.255.255.252::eth0:off")
}

fn ruleset() -> String {
    format!(
        r#"table inet zapper {{
  chain prerouting {{
    type nat hook prerouting priority dstnat; policy accept;
    iifname "{TAP}" meta l4proto tcp redirect to :{TCP_PORT}
    iifname "{TAP}" udp dport 53 redirect to :{DNS_PORT}
  }}
  chain input {{
    type filter hook input priority filter; policy drop;
    iifname "lo" accept
    iifname "{TAP}" tcp dport {TCP_PORT} accept
    iifname "{TAP}" udp dport {DNS_PORT} accept
    ct state established,related accept
  }}
  chain forward {{
    type filter hook forward priority filter; policy drop;
  }}
}}
"#
    )
}

fn tool(name: &str) -> PathBuf {
    for dir in ["/usr/sbin", "/sbin", "/usr/bin", "/bin"] {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(name)
}

fn run(name: &str, args: &[&str], stdin: Option<&str>) -> Result<()> {
    let mut cmd = Command::new(tool(name));
    cmd.args(args).stdout(Stdio::null()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().with_context(|| format!("running {name} (install iproute2 / nftables)"))?;
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{name} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Enters a new user and network namespace, as root of both, and builds the
/// guest's network.
fn enter() -> Result<()> {
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) } != 0 {
        let e = io::Error::last_os_error();
        bail!("unshare(user, net): {e} (unprivileged user namespaces disabled? see docs/security.md)");
    }
    std::fs::write("/proc/self/setgroups", "deny")?;
    std::fs::write("/proc/self/uid_map", format!("0 {uid} 1"))?;
    std::fs::write("/proc/self/gid_map", format!("0 {gid} 1"))?;
    run("ip", &["link", "set", "lo", "up"], None)?;
    run("ip", &["tuntap", "add", TAP, "mode", "tap"], None)?;
    run("ip", &["addr", "add", &format!("{GATEWAY}/30"), "dev", TAP], None)?;
    run("ip", &["link", "set", TAP, "up"], None)?;
    run("nft", &["-f", "-"], Some(&ruleset()))?;
    Ok(())
}

/// `zapper __netns --probe`: can this host build the namespace?
pub fn probe() -> i32 {
    match enter() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

/// `zapper __netns DIR FIRECRACKER CONFIG ID`
pub fn main(args: &[String]) -> i32 {
    let [dir, firecracker, config, id] = args else {
        eprintln!("usage: zapper __netns DIR FIRECRACKER CONFIG ID");
        return 2;
    };
    let dir = PathBuf::from(dir);
    // Entering the user namespace changes credentials, which clears the
    // parent-death signal the daemon set: set it again.
    let parent = unsafe { libc::getppid() };
    if let Err(e) = enter() {
        eprintln!("zapper netns: {e:#}");
        return 1;
    }
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
    if unsafe { libc::getppid() } != parent {
        return 1;
    }
    if let Err(e) = serve(&dir) {
        eprintln!("zapper netns: {e:#}");
        return 1;
    }
    let status = unsafe {
        Command::new(firecracker)
            .args(["--no-api", "--config-file", config, "--level", "Warn", "--id", id])
            .current_dir(&dir)
            .stdin(Stdio::null())
            .pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            })
            .status()
    };
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("zapper netns: starting {firecracker}: {e}");
            1
        }
    }
}

fn serve(dir: &Path) -> Result<()> {
    let socket = dir.join(SOCKET);
    let tcp = TcpListener::bind(SocketAddrV4::new(GATEWAY, TCP_PORT))?;
    let udp = UdpSocket::bind(SocketAddrV4::new(GATEWAY, DNS_PORT))?;
    let s = socket.clone();
    thread::spawn(move || {
        for conn in tcp.incoming().flatten() {
            let s = s.clone();
            thread::spawn(move || {
                let _ = relay_tcp(conn, &s);
            });
        }
    });
    thread::spawn(move || {
        let mut buf = [0u8; 1500];
        loop {
            let Ok((n, peer)) = udp.recv_from(&mut buf) else { continue };
            let (query, socket, udp) = (buf[..n].to_vec(), socket.clone(), udp.try_clone());
            thread::spawn(move || {
                if let (Ok(answer), Ok(udp)) = (relay_dns(&query, &socket), udp) {
                    let _ = udp.send_to(&answer, peer);
                }
            });
        }
    });
    Ok(())
}

/// The destination the guest dialled, before the redirect.
fn original_dst(conn: &TcpStream) -> io::Result<SocketAddrV4> {
    const SO_ORIGINAL_DST: libc::c_int = 80;
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(conn.as_raw_fd(), libc::SOL_IP, SO_ORIGINAL_DST, &mut addr as *mut _ as *mut libc::c_void, &mut len) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SocketAddrV4::new(Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)), u16::from_be(addr.sin_port)))
}

fn relay_tcp(conn: TcpStream, socket: &Path) -> io::Result<()> {
    let dst = original_dst(&conn)?;
    let mut up = UnixStream::connect(socket)?;
    up.write_all(format!("TCP {} {}\n", dst.ip(), dst.port()).as_bytes())?;
    let (mut cr, mut uw) = (conn.try_clone()?, up.try_clone()?);
    let t = thread::spawn(move || {
        let _ = io::copy(&mut cr, &mut uw);
        let _ = uw.shutdown(std::net::Shutdown::Write);
    });
    let (mut ur, mut cw) = (up, conn);
    let _ = io::copy(&mut ur, &mut cw);
    let _ = cw.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
    Ok(())
}

fn relay_dns(query: &[u8], socket: &Path) -> io::Result<Vec<u8>> {
    let mut up = UnixStream::connect(socket)?;
    up.set_read_timeout(Some(Duration::from_secs(5)))?;
    up.write_all(format!("DNS {}\n", query.len()).as_bytes())?;
    up.write_all(query)?;
    let mut len = [0u8; 2];
    up.read_exact(&mut len)?;
    let mut answer = vec![0u8; u16::from_be_bytes(len) as usize];
    up.read_exact(&mut answer)?;
    Ok(answer)
}
