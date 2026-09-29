//! The agent skill embedded in the binary, and its installed copies.
//!
//! `hex skill install` writes the skill to `~/.claude/skills/hex/` and
//! `~/.agents/skills/hex/`, with `metadata.hex-version` stamped into the
//! SKILL.md frontmatter. A stamped copy is hex-owned: [`refresh`] rewrites it
//! when the binary's version differs. A symlink (a dev checkout) and an
//! unstamped copy (`npx skills add`, hand edits) belong to someone else and are
//! left alone.

use std::path::{Path, PathBuf};

use crate::error::{HexError, Result};

/// The binary version stamped into an installed copy.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The skill's files, as paths relative to the skill dir. SKILL.md is stamped on
/// write; add a new file here with its own `include_str!`.
const FILES: &[(&str, &str)] = &[("SKILL.md", include_str!("../skill/hex/SKILL.md"))];

/// Where an installed copy lives, relative to `$HOME`.
const TARGETS: [&str; 2] = [".claude/skills/hex", ".agents/skills/hex"];

/// What sits at one target dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Nothing, or a dir with no SKILL.md.
    Missing,
    /// A symlink, with its target.
    Symlink(PathBuf),
    /// A SKILL.md with no `hex-version` stamp.
    Unstamped,
    /// A stamped SKILL.md, with the stamped version.
    Stamped(String),
}

/// The target dirs, as `(~/… display name, absolute path)`. Empty when `HOME`
/// is not set.
#[must_use]
pub fn targets() -> Vec<(String, PathBuf)> {
    let Some(home) = crate::local_log::home_dir() else {
        return Vec::new();
    };
    TARGETS
        .iter()
        .map(|rel| (format!("~/{rel}"), home.join(rel)))
        .collect()
}

/// Inspect one target dir.
///
/// # Errors
/// Fails if the dir or its SKILL.md exists but cannot be read.
pub fn inspect(dir: &Path) -> std::io::Result<Found> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Ok(Found::Symlink(std::fs::read_link(dir)?));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Found::Missing),
        Err(e) => return Err(e),
    }
    match std::fs::read_to_string(dir.join("SKILL.md")) {
        Ok(text) => Ok(stamp_of(&text).map_or(Found::Unstamped, Found::Stamped)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Found::Missing),
        Err(e) => Err(e),
    }
}

/// What `hex skill install` did to one target dir.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// Written where nothing was.
    Installed {
        /// The stamped version.
        version: String,
    },
    /// A stamped copy of another version, rewritten.
    Updated {
        /// The version it had.
        from: String,
        /// The version it has now.
        to: String,
    },
    /// A stamped copy of this version, left as is.
    UpToDate {
        /// The stamped version.
        version: String,
    },
    /// Left as is, for the reason given.
    Skipped {
        /// Why.
        why: String,
    },
}

/// One target dir and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Installed {
    /// The dir, as `~/…`.
    pub path: String,
    /// What happened.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Install the skill into every target dir. `force` overwrites an unstamped
/// copy; nothing replaces a symlink.
///
/// # Errors
/// Fails if `HOME` is not set, or a target cannot be read or written.
pub fn install(force: bool) -> Result<Vec<Installed>> {
    let targets = targets();
    if targets.is_empty() {
        return Err(HexError::new(
            "HOME is not set, cannot locate ~/.claude/skills",
        ));
    }
    let mut report = Vec::new();
    for (path, dir) in targets {
        let io = |e: std::io::Error| HexError::new(format!("{path}: {e}"));
        let outcome = match inspect(&dir).map_err(io)? {
            Found::Symlink(to) => Outcome::Skipped {
                why: format!("symlink → {}, left as is", to.display()),
            },
            Found::Unstamped if !force => Outcome::Skipped {
                why: "not installed by hex (no hex-version stamp); --force overwrites it".into(),
            },
            Found::Stamped(v) if v == VERSION => Outcome::UpToDate { version: v },
            Found::Stamped(from) => {
                write(&dir).map_err(io)?;
                Outcome::Updated {
                    from,
                    to: VERSION.to_owned(),
                }
            }
            Found::Missing | Found::Unstamped => {
                write(&dir).map_err(io)?;
                Outcome::Installed {
                    version: VERSION.to_owned(),
                }
            }
        };
        report.push(Installed { path, outcome });
    }
    Ok(report)
}

/// Rewrite each stamped copy whose version differs from the binary's. Never
/// installs, and never fails: returns one stderr line per copy it refreshed or
/// could not read or write.
#[must_use]
pub fn refresh() -> Vec<String> {
    let mut lines = Vec::new();
    for (path, dir) in targets() {
        match inspect(&dir) {
            Ok(Found::Stamped(from)) if from != VERSION => match write(&dir) {
                Ok(()) => lines.push(format!("refreshed skill {path} ({from} → {VERSION})")),
                Err(e) => lines.push(format!("hex: cannot refresh skill {path}: {e}")),
            },
            Ok(_) => {}
            Err(e) => lines.push(format!("hex: cannot read skill {path}: {e}")),
        }
    }
    lines
}

/// Whether any target dir holds a stamped copy.
#[must_use]
pub fn installed() -> bool {
    targets()
        .iter()
        .any(|(_, dir)| matches!(inspect(dir), Ok(Found::Stamped(_))))
}

/// One `hex doctor` row per target dir. `ok` means a stamped copy; the rows are
/// informational and do not fail doctor.
#[must_use]
pub fn findings() -> Vec<crate::doctor::Finding> {
    targets()
        .into_iter()
        .map(|(name, dir)| {
            let (ok, detail) = match inspect(&dir) {
                Ok(Found::Stamped(v)) => (true, v),
                Ok(Found::Symlink(to)) => (false, format!("symlink → {}", to.display())),
                Ok(Found::Missing) => (false, "not installed".to_owned()),
                Ok(Found::Unstamped) => (false, "unstamped".to_owned()),
                Err(e) => (false, format!("unreadable: {e}")),
            };
            crate::doctor::Finding {
                kind: "skill",
                name,
                program: None,
                ok,
                detail,
            }
        })
        .collect()
}

/// Write every skill file into `dir`, SKILL.md stamped.
fn write(dir: &Path) -> std::io::Result<()> {
    for (rel, text) in FILES {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if *rel == "SKILL.md" {
            std::fs::write(path, stamp(text, VERSION))?;
        } else {
            std::fs::write(path, text)?;
        }
    }
    Ok(())
}

/// Split `text` into its YAML frontmatter and the rest, if it has one.
fn frontmatter(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix("---\n")?;
    let end = body.find("\n---")?;
    Some((&body[..=end], &body[end + 1..]))
}

/// `text` with `metadata.hex-version: "<version>"` in its frontmatter: merged
/// into an existing top-level `metadata:` key, else added before the closing
/// `---`.
fn stamp(text: &str, version: &str) -> String {
    let line = format!("  hex-version: \"{version}\"\n");
    let Some((front, rest)) = frontmatter(text) else {
        return format!("---\nmetadata:\n{line}---\n{text}");
    };
    let front = match front.find("\nmetadata:\n") {
        Some(at) => {
            let split = at + "\nmetadata:\n".len();
            format!("{}{line}{}", &front[..split], &front[split..])
        }
        None if front.starts_with("metadata:\n") => {
            format!("metadata:\n{line}{}", &front["metadata:\n".len()..])
        }
        None => format!("{front}metadata:\n{line}"),
    };
    format!("---\n{front}{rest}")
}

/// The `metadata.hex-version` in `text`'s frontmatter.
fn stamp_of(text: &str) -> Option<String> {
    let (front, _) = frontmatter(text)?;
    let yaml: yaml_serde::Value = yaml_serde::from_str(front).ok()?;
    Some(
        yaml.get("metadata")?
            .get("hex-version")?
            .as_str()?
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_adds_metadata_before_the_closing_fence() {
        let out = stamp("---\nname: hex\n---\n# body\n", "1.2.3");
        assert_eq!(
            out,
            "---\nname: hex\nmetadata:\n  hex-version: \"1.2.3\"\n---\n# body\n"
        );
        assert_eq!(stamp_of(&out).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn stamp_merges_into_an_existing_metadata_key() {
        let out = stamp(
            "---\nname: hex\nmetadata:\n  author: k\nlicense: MIT\n---\nbody\n",
            "1.2.3",
        );
        assert_eq!(
            out,
            "---\nname: hex\nmetadata:\n  hex-version: \"1.2.3\"\n  author: k\nlicense: MIT\n---\nbody\n"
        );
        assert_eq!(stamp_of(&out).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn the_embedded_skill_is_unstamped_and_stamps_cleanly() {
        let (_, skill) = FILES[0];
        assert_eq!(stamp_of(skill), None);
        assert_eq!(stamp_of(&stamp(skill, VERSION)).as_deref(), Some(VERSION));
    }
}
