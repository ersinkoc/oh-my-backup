//! Declarative, serializable file rules. No embedded scripts or credentials.
use anyhow::{Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileKind {
    #[default]
    Veeam,
    Files,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Deletion {
    #[default]
    Keep,
    Quarantine,
    Mirror,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileRules {
    pub recursive: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub quiet_seconds: u64,
    pub deletion: Deletion,
    pub keep_versions: usize,
    pub overwrite_unmanaged: bool,
    pub protect_local_changes: bool,
}

impl Default for FileRules {
    fn default() -> Self {
        Self {
            recursive: true,
            include: vec!["**".into()],
            exclude: vec!["**/*.tmp".into(), "**/*.part".into(), "**/*.lock".into()],
            quiet_seconds: 60,
            deletion: Deletion::Keep,
            keep_versions: 5,
            overwrite_unmanaged: false,
            protect_local_changes: true,
        }
    }
}

pub struct Matcher {
    include: Vec<Regex>,
    exclude: Vec<Regex>,
    recursive: bool,
}

impl FileRules {
    pub fn compile(&self) -> Result<Matcher> {
        if self.include.is_empty() || self.include.len() + self.exclude.len() > 128 {
            bail!("Provide 1–128 include/exclude patterns");
        }
        if self.quiet_seconds > 86400 || !(1..=100).contains(&self.keep_versions) {
            bail!("Quiet time must be at most one day; retain 1–100 versions");
        }
        Ok(Matcher {
            include: self
                .include
                .iter()
                .map(|s| glob(s))
                .collect::<Result<_>>()?,
            exclude: self
                .exclude
                .iter()
                .map(|s| glob(s))
                .collect::<Result<_>>()?,
            recursive: self.recursive,
        })
    }
}

fn glob(pattern: &str) -> Result<Regex> {
    if pattern.is_empty()
        || pattern.len() > 512
        || pattern.starts_with('/')
        || pattern.contains(['\\', ':', '\0', '[', ']'])
        || pattern.split('/').any(|p| p == ".." || p == ".")
    {
        bail!("Invalid glob: use relative paths with *, ** or ?");
    }
    let chars = pattern.chars().collect::<Vec<_>>();
    let mut result = String::from("^");
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 1;
                if chars.get(i + 1) == Some(&'/') {
                    result.push_str("(?:.*/)?");
                    i += 1;
                } else {
                    result.push_str(".*");
                }
            }
            '*' => result.push_str("[^/]*"),
            '?' => result.push_str("[^/]"),
            c => result.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    result.push('$');
    Ok(Regex::new(&result)?)
}

impl Matcher {
    pub fn matches(&self, path: &str) -> bool {
        (self.recursive || !path.contains('/'))
            && self.include.iter().any(|r| r.is_match(path))
            && !self.exclude.iter().any(|r| r.is_match(path))
    }
}

pub fn validate_relative(path: &str) -> Result<()> {
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':']) {
        bail!("Unsafe relative file path");
    }
    for (index, component) in path.split('/').enumerate() {
        let stem = component.split('.').next().unwrap_or("").to_uppercase();
        let device = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.ends_with([' ', '.'])
            || component
                .chars()
                .any(|c| c.is_control() || "<>\"|?*".contains(c))
            || device
            || (index == 0 && component.eq_ignore_ascii_case(".hbsync"))
        {
            bail!("Unsupported or reserved Windows file path");
        }
    }
    Ok(())
}

pub fn validate_paths<'a>(names: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for name in names {
        validate_relative(name)?;
        if !seen.insert(name.to_uppercase()) {
            bail!("Source names collide on Windows");
        }
    }
    // A file cannot also be the parent directory of another file.
    for name in &seen {
        let mut path = name.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            if seen.contains(parent) {
                bail!("Source file/directory collision");
            }
            path = parent;
        }
    }
    Ok(())
}

pub fn plain_path(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(m) if m.file_attributes() & 0x400 != 0 => {
                bail!("Destination contains a link or junction")
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub fn target(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative(relative)?;
    let path = root.join(relative);
    plain_path(&path)?;
    Ok(path)
}

pub fn normalized_root(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        bail!("Target must be absolute");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("Target escapes its root");
                }
            }
            Component::CurDir => {}
            c => normalized.push(c.as_os_str()),
        }
    }
    let mut ancestor = normalized.clone();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid target"))?
                .to_owned(),
        );
        ancestor.pop();
    }
    let mut resolved = ancestor.canonicalize()?;
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved
        .to_string_lossy()
        .trim_end_matches('\\')
        .to_lowercase())
}

pub fn overlaps(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}\\")) || b.starts_with(&format!("{a}\\"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn globs_cover_root_and_recursive_paths() {
        let rules = FileRules::default();
        let matcher = rules.compile().unwrap();
        assert!(matcher.matches("report.txt"));
        assert!(matcher.matches("a/b/report.txt"));
        assert!(!matcher.matches("a.tmp"));
        assert!(!matcher.matches("a/b/file.lock"));
        let rules = FileRules {
            include: vec!["data/**/*.csv".into()],
            ..rules
        };
        let matcher = rules.compile().unwrap();
        assert!(matcher.matches("data/root.csv"));
        assert!(matcher.matches("data/a/b.csv"));
        assert!(!matcher.matches("data/b.txt"));
    }
    #[test]
    fn paths_reject_escape_reserved_names_and_collisions() {
        for path in [
            "../x",
            "a/../x",
            "a\\x",
            "/root",
            "a:b",
            ".HBSYNC/state.json",
            "con.txt",
            "a/LPT1",
            "a./x",
            "a//b",
            "x ",
        ] {
            assert!(validate_relative(path).is_err(), "{path}");
        }
        assert!(validate_paths(["a.txt", "A.TXT"].into_iter()).is_err());
        assert!(validate_paths(["a", "a/b"].into_iter()).is_err());
        assert!(validate_paths(["root.txt", "a/b.txt"].into_iter()).is_ok());
    }
}
