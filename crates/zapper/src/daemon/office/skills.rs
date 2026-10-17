//! Workers' skills: built into the binary from `config/skills/`, or read
//! from a firm's `skills_dir` (relative to the config file). Each role's
//! three skills are copied into the run and attached to its workers
//! read-only at `~/.claude/skills`.

use anyhow::{anyhow, Context, Result};
use zapper_policy::{expand_home, Config};

use super::FirmRun;

macro_rules! builtin {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../../../../../config/skills/", $name, "/SKILL.md")))),*]
    };
}

pub const BUILTIN: &[(&str, &str)] = builtin![
    "office-mail",
    "office-publish",
    "office-resources",
    "ceo",
    "product-manager",
    "manager",
    "cto",
    "software-engineer",
    "designer",
    "researcher",
    "analyst",
    "security-review",
];

/// The skill officers work by.
const SECURITY: &str = "security-review";
/// The skill of every role that hires.
const MANAGER: &str = "manager";
/// Every worker's training: the firm's resources and how to get access.
const RESOURCES: &str = "office-resources";

/// A skill's `SKILL.md`: from the firm's directory if it has one, else built in.
pub fn text(dir: Option<&str>, name: &str) -> Result<String> {
    if let Some(dir) = dir {
        let path = std::path::Path::new(&expand_home(dir))
            .join(name)
            .join("SKILL.md");
        if path.is_file() {
            return std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()));
        }
    }
    BUILTIN
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.to_string())
        .ok_or_else(|| {
            anyhow!(
                "no skill {name} (built in: {})",
                BUILTIN.iter().map(|b| b.0).collect::<Vec<_>>().join(", ")
            )
        })
}

/// Writes `skills/<role>/<skill>/SKILL.md` in the run directory.
pub fn prepare(run: &FirmRun, _config: &Config) -> Result<()> {
    let dir = run.config.skills_dir.as_deref();
    // The hierarchy: everyone's shared skills, the role's own work skill,
    // the manager skill for roles that hire, and the security skill for
    // officers and the CEO (who holds security until an officer is hired).
    let root_role = run
        .config
        .root()
        .and_then(|r| run.config.worker(r))
        .map(|w| w.role.clone());
    for (role_name, role) in &run.config.roles {
        let mut names = role.skills.names();
        names.push(RESOURCES);
        let mut add = |n: &'static str| {
            if !names.contains(&n) {
                names.push(n);
            }
        };
        if !role.hires.is_empty() {
            add(MANAGER);
        }
        if role.officer || root_role.as_deref() == Some(role_name.as_str()) {
            add(SECURITY);
        }
        for skill in names {
            let out = run.dir.join("skills").join(role_name).join(skill);
            std::fs::create_dir_all(&out)?;
            std::fs::write(
                out.join("SKILL.md"),
                text(dir, skill).with_context(|| format!("role {role_name}"))?,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtins_have_frontmatter() {
        for (name, text) in super::BUILTIN {
            assert!(text.starts_with("---\n"), "{name}");
            assert!(text.contains(&format!("name: {name}\n")), "{name}");
        }
    }
}
