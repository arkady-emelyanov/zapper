//! Root filesystem images from OCI images.
//!
//! An image is an ext4 file made from a container image's filesystem. The
//! extraction runs inside the container engine's user namespace
//! (`podman unshare`), so file ownership comes out as in the image without
//! root. The guest agent is not part of the image: it boots from an
//! initramfs the daemon builds.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::daemon::vm::parse_size;

fn engine() -> String {
    std::env::var("ZAPPER_OCI").unwrap_or_else(|_| "podman".into())
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("running {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub struct Build<'a> {
    pub name: &'a str,
    pub from: &'a str,
    /// Shell commands run in the image at build time, as `RUN` steps.
    pub run: &'a [String],
    pub size: Option<&'a str>,
    pub dir: &'a Path,
}

pub fn build(b: Build<'_>) -> Result<PathBuf> {
    if !b
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        bail!(
            "image name {:?}: use letters, digits, '-', '_' and '.'",
            b.name
        );
    }
    std::fs::create_dir_all(b.dir)?;
    let oci = engine();
    let work = tempdir()?;
    let reference = if b.run.is_empty() {
        eprintln!("pulling {}", b.from);
        run(Command::new(&oci).args(["pull", "-q", b.from]))?;
        b.from.to_string()
    } else {
        let mut file = format!("FROM {}\n", b.from);
        for r in b.run {
            file.push_str(&format!("RUN {r}\n"));
        }
        std::fs::write(work.join("Containerfile"), file)?;
        let tag = format!("localhost/zapper-image-{}", b.name);
        eprintln!("building {tag} from {}", b.from);
        let status = Command::new(&oci)
            .args(["build", "-t", &tag, "-f"])
            .arg(work.join("Containerfile"))
            .arg(&work)
            .status()?;
        if !status.success() {
            bail!("{oci} build failed");
        }
        tag
    };
    let cid = run(Command::new(&oci).args(["create", &reference, "/bin/true"]))?;
    let tar = work.join("rootfs.tar");
    let exported = run(Command::new(&oci).args(["export", &cid, "-o"]).arg(&tar));
    let _ = run(Command::new(&oci).args(["rm", "-f", &cid]));
    exported?;

    let tar_size = std::fs::metadata(&tar)?.len();
    let size = match b.size {
        Some(s) => parse_size(s)?,
        None => tar_size + tar_size / 4 + (256 << 20),
    };
    let out = b.dir.join(format!("{}.ext4", b.name));
    let tmp = b.dir.join(format!(".{}.ext4.tmp", b.name));
    let root = work.join("root");
    eprintln!("writing {} ({} MiB)", out.display(), size >> 20);
    let script = r#"set -e; mkdir "$1"; tar -C "$1" -xf "$2"; mkfs.ext4 -q -F -L zapper -d "$1" "$3" "$4"; rm -rf "$1""#;
    let mut unshare = Command::new(&oci);
    unshare
        .args(["unshare", "sh", "-c", script, "sh"])
        .arg(&root)
        .arg(&tar)
        .arg(&tmp)
        .arg(format!("{}k", size / 1024));
    run(&mut unshare)?;
    std::fs::rename(&tmp, &out)?;
    let meta = json!({ "name": b.name, "from": b.from, "run": b.run, "size": size, "built": crate::daemon::now() as u64 });
    std::fs::write(
        b.dir.join(format!("{}.json", b.name)),
        serde_json::to_vec_pretty(&meta)?,
    )?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(out)
}

pub fn list(dir: &Path) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(out);
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_some_and(|x| x == "ext4") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let meta: serde_json::Value = std::fs::read(path.with_extension("json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(json!({}));
            out.push(json!({ "name": name, "from": meta["from"], "size": e.metadata()?.len(), "built": meta["built"] }));
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(out)
}

fn tempdir() -> Result<PathBuf> {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/var/tmp".into());
    let dir = PathBuf::from(base).join(format!("zapper-image-{}", crate::daemon::random_id(8)));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
