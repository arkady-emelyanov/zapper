//! `zapper-keeper`: keeps zapper's record append-only. Runs as its own user
//! (see `scripts/install-keeper.sh`); takes appends from one uid only.
//!
//!   zapper-keeper serve --socket PATH --dir DIR --key PATH --writer-uid UID [--group GID] [--ui ADDR]
//!   zapper-keeper verify FILE STREAM PUBLIC_KEY_HEX

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use zapper_keeper::{load_key, peer_uid, verify, Keeper};

fn arg(args: &[String], name: &str) -> Result<String> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
        .with_context(|| format!("{name} is required"))
}

fn serve(args: &[String]) -> Result<()> {
    let socket = PathBuf::from(arg(args, "--socket")?);
    let dir = PathBuf::from(arg(args, "--dir")?);
    let key_path = PathBuf::from(arg(args, "--key")?);
    let writer: u32 = arg(args, "--writer-uid")?.parse().context("--writer-uid")?;
    // With the writer's group, only they can read the record (the keeper's
    // user is a member of it); without, everyone can.
    let group: Option<u32> = match arg(args, "--group") {
        Ok(g) => Some(g.parse().context("--group")?),
        Err(_) => None,
    };
    unsafe { libc::umask(if group.is_some() { 0o027 } else { 0o022 }) };
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(
        &dir,
        std::fs::Permissions::from_mode(if group.is_some() { 0o750 } else { 0o755 }),
    )?;
    if let Some(g) = group {
        zapper_keeper::give(&std::fs::File::open(&dir)?, g)?;
    }
    let min_free = match arg(args, "--min-free-mib") {
        Ok(m) => m.parse::<u64>().context("--min-free-mib")? << 20,
        Err(_) => zapper_keeper::MIN_FREE,
    };
    let keeper = Arc::new(
        Keeper::new(&dir, load_key(&key_path, &dir)?)
            .with_group(group)
            .with_min_free(min_free),
    );
    if let Ok(addr) = arg(args, "--ui") {
        zapper_keeper::ui::start(keeper.clone(), &addr, writer, group)?;
    }
    let _ = std::fs::remove_file(&socket);
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    // Anyone may connect; only the writer's appends are taken.
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))?;
    eprintln!(
        "zapper-keeper: keeping {} for uid {writer}, on {}; public key {}",
        dir.display(),
        socket.display(),
        keeper.public_key()
    );
    {
        let k = keeper.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            k.tick();
        });
    }
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        match peer_uid(&conn) {
            Ok(uid) if uid == writer => {
                let k = keeper.clone();
                std::thread::spawn(move || k.serve(conn));
            }
            Ok(uid) => eprintln!("zapper-keeper: refused a connection from uid {uid}"),
            Err(e) => eprintln!("zapper-keeper: {e:#}"),
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let r = match args.get(1).map(String::as_str) {
        Some("serve") => serve(&args[2..]),
        Some("verify") if args.len() == 5 => verify(args[2].as_ref(), &args[3], &args[4]).map(|r| println!("{}", serde_json::to_string(&r).unwrap())),
        _ => Err(anyhow::anyhow!("usage: zapper-keeper serve --socket PATH --dir DIR --key PATH --writer-uid UID\n       zapper-keeper verify FILE STREAM PUBLIC_KEY_HEX")),
    };
    if let Err(e) = r {
        eprintln!("zapper-keeper: {e:#}");
        std::process::exit(1);
    }
}
