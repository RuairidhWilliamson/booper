#![warn(missing_docs)]

//! Booper is a cli tool to increment version numbers for projects commit them, tag a release and push it using git.
//!
//! The main use case is that you have a project that you want to release a new version this involves changing the Cargo.toml version number.
//! Running `cargo check` to update the Cargo.lock file and incrementing any other places the version is mentioned.
//! Committing this as a new change, tagging it and pushing it.
//!
//! Booper simplifies this into one simple command `booper -ctp` or `booper -ctp minor`
//!
//! Booper will search for versions in common places and ask if you want to increment them.
//!
//! Currently booper only checks `Cargo.toml` and `.env` but this is likely to expand in the future.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::{path::Path, str::FromStr};

use clap::Parser;
use regex::{Captures, Regex};
use semver::Version;

fn main() -> Result<(), ()> {
    let cli = Cli::parse();
    cli.boop()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionIncrement {
    Auto,
    Patch,
    Minor,
    Major,
    StripPrerelease,
    Prerelease,
    Exact(Version),
}

impl FromStr for VersionIncrement {
    type Err = semver::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_ref() {
            "auto" => Self::Auto,
            "patch" => Self::Patch,
            "minor" => Self::Minor,
            "major" => Self::Major,
            "strip" => Self::StripPrerelease,
            "pre" => Self::Prerelease,
            _ => Self::Exact(Version::from_str(s)?),
        })
    }
}

impl VersionIncrement {
    fn increment(&self, current: &Version) -> Version {
        match self {
            Self::Auto => {
                if current.pre.is_empty() {
                    Self::Patch.increment(current)
                } else {
                    Self::StripPrerelease.increment(current)
                }
            }
            Self::Patch => Version {
                patch: current.patch + 1,
                ..current.clone()
            },
            Self::Minor => Version {
                patch: 0,
                minor: current.minor + 1,
                ..current.clone()
            },
            Self::Major => Version {
                patch: 0,
                minor: 0,
                major: current.major + 1,
                ..current.clone()
            },
            Self::StripPrerelease => Version {
                pre: semver::Prerelease::default(),
                ..current.clone()
            },
            Self::Prerelease => Version {
                pre: semver::Prerelease::new("pre").unwrap(),
                ..current.clone()
            },
            Self::Exact(version) => version.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Precise,
    Loose,
    Skip,
}

impl FileKind {
    fn new(path: &Path) -> Self {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("Cargo.toml") => Self::Precise,
            Some("Cargo.lock") => Self::Skip,
            _ => Self::Loose,
        }
    }
}

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Can be one of `patch`, `minor`, `major`, `strip`, `pre` or an exact version e.g. `1.0.3`
    ///
    /// Defaults to `patch` or `strip` for prerelease
    #[arg(default_value = "auto")]
    increment: VersionIncrement,

    /// Whether or not to commit the version changes
    #[arg(short, long)]
    commit: bool,

    /// Whether or not to tag the commit. Requires -c / --commit
    #[arg(short, long)]
    tag: bool,

    /// Whether or not to push the commit and tag. Requires -c / --commit
    #[arg(short, long)]
    push: bool,

    /// Skips the interactive confirm step
    #[arg(short = 'y', long)]
    force: bool,
}

impl Cli {
    fn find_current_version() -> Result<Version, ()> {
        let general_precise_regex = Regex::new("((VERSION|version) ?= ?)\"([^\"]+)\"").unwrap();
        let files = ["Cargo.toml", ".env"];
        let versions: Vec<String> = files
            .into_iter()
            .map(Path::new)
            .filter_map(|file| {
                let contents = std::fs::read_to_string(file).ok()?;
                let cap = general_precise_regex.captures(&contents)?;
                Some(cap.get(3)?.as_str().to_owned())
            })
            .collect();
        assert(!versions.is_empty(), "no versions found")?;
        assert(
            all_equal(&versions),
            &format!("no consistent version found: {versions:?}"),
        )?;
        let from_version = semver::Version::parse(&versions[0]).unwrap();
        Ok(from_version)
    }

    fn boop(&self) -> Result<(), ()> {
        assert_git_clean()?;
        let from_version = Self::find_current_version()?;
        let last_tag = get_last_tag();
        if let Some(last_tag) = &last_tag {
            let stripped_last_tag = last_tag.strip_prefix('v').unwrap_or(last_tag);
            if !stripped_last_tag.is_empty()
                && from_version.pre.is_empty()
                && from_version != semver::Version::parse(stripped_last_tag).unwrap()
            {
                eprintln!("last git tag does not match the detected tag");
                return Err(());
            }
        }
        assert(from_version.build.is_empty(), "build suffix unsupported")?;
        let to_version = self.increment.increment(&from_version);

        eprintln!("Upgrading version {from_version} to {to_version}");

        let precise_regex = regex::Regex::new(&format!(
            "((VERSION|version) ?= ?)\"(?<replace>{from_version})\"",
            from_version = regex::escape(&from_version.to_string())
        ))
        .unwrap();
        let loose_regex = regex::Regex::new(&format!(
            "\\b(?<replace>{from_version})\\b",
            from_version = regex::escape(&from_version.to_string())
        ))
        .unwrap();
        let to_version = ToVersion {
            string: to_version.to_string(),
            precise_regex,
            loose_regex,
        };

        let matching_files = to_version.find_files_to_update();
        let mut ops = Vec::new();
        if self.commit {
            ops.push("committed");
            if self.tag {
                ops.push("tagged");
            }
            if self.push {
                ops.push("pushed");
            }
        }
        let ops_display = if let Some(last) = ops.pop() {
            let mut output = String::new();
            for x in ops {
                let _ = write!(output, ", {x}");
            }
            let _ = write!(output, " and {last}");
            output
        } else {
            String::new()
        };
        eprintln!("The following files will be changed{ops_display}:");
        for file in &matching_files {
            eprintln!("\t{}", file.display());
        }
        if !self.force
            && !dialoguer::Confirm::new()
                .with_prompt("Do you want to continue?")
                .interact()
                .unwrap()
        {
            return Err(());
        }

        to_version.update_files(&matching_files);

        cargo_check()?;
        eprintln!("Upgraded!");
        self.git_operations(&to_version, last_tag)
    }

    fn git_operations(&self, to_version: &ToVersion, last_tag: Option<String>) -> Result<(), ()> {
        let to_version_tag = last_tag
            .map(|last_tag| {
                if last_tag.starts_with('v') {
                    format!("v{}", &to_version.string)
                } else {
                    to_version.string.clone()
                }
            })
            .unwrap_or_else(|| format!("v{}", &to_version.string));
        if self.commit {
            let msg = format!("Version {}", &to_version.string);
            commit(&msg)?;
            if self.push {
                push()?;
            }

            if self.tag {
                tag(&to_version_tag)?;
                if self.push {
                    push_tag(&to_version_tag)?;
                }
            }
        } else {
            if self.tag {
                eprintln!("Can't tag when -c / --commit is not enabled");
                return Err(());
            }
            if self.push {
                eprintln!("Can't push when -c / --commit is not enabled");
                return Err(());
            }
        }
        Ok(())
    }
}

struct ToVersion {
    string: String,
    precise_regex: Regex,
    loose_regex: Regex,
}

impl ToVersion {
    fn find_files_to_update(&self) -> Vec<PathBuf> {
        ignore::Walk::new(".")
            .filter_map(|entry| {
                let entry = entry.unwrap();
                if !entry.file_type()?.is_file() {
                    return None;
                }
                let file = entry.path();
                let regex = match FileKind::new(file) {
                    FileKind::Precise => &self.precise_regex,
                    FileKind::Loose => &self.loose_regex,
                    FileKind::Skip => {
                        return None;
                    }
                };
                let contents = std::fs::read_to_string(file).ok()?;
                if regex.is_match(&contents) {
                    Some(file.to_path_buf())
                } else {
                    None
                }
            })
            .collect()
    }

    fn update_files(&self, matching_files: &[PathBuf]) {
        for file in matching_files {
            let regex = match FileKind::new(file) {
                FileKind::Precise => &self.precise_regex,
                FileKind::Loose => &self.loose_regex,
                FileKind::Skip => continue,
            };
            let contents = std::fs::read_to_string(file).unwrap();
            let replaced_contents = regex.replace_all(&contents, |caps: &Captures| {
                caps.get_match()
                    .as_str()
                    .replace(caps.name("replace").unwrap().as_str(), &self.string)
            });
            std::fs::write(file, replaced_contents.as_ref()).unwrap();
        }
    }
}

fn all_equal<T: Eq>(v: &[T]) -> bool {
    let first = v.first();
    if first.is_none() {
        return false;
    }
    for x in &v[1..] {
        if Some(x) != first {
            return false;
        }
    }
    true
}

fn assert(check: bool, err: &str) -> Result<(), ()> {
    if check {
        Ok(())
    } else {
        eprintln!("{err}");
        Err(())
    }
}

fn cargo_check() -> Result<(), ()> {
    assert(
        std::process::Command::new("cargo")
            .args(["check", "-q"])
            .status()
            .unwrap()
            .success(),
        "cargo check failed",
    )
}

fn assert_git_clean() -> Result<(), ()> {
    assert(
        std::process::Command::new("git")
            .args(["diff", "--exit-code"])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "uncommitted changes",
    )
}

fn commit(message: &str) -> Result<(), ()> {
    assert(
        std::process::Command::new("git")
            .args(["commit", "-am", message])
            .status()
            .unwrap()
            .success(),
        "commit failed",
    )
}

fn push() -> Result<(), ()> {
    assert(
        std::process::Command::new("git")
            .args(["push"])
            .status()
            .unwrap()
            .success(),
        "push failed",
    )
}

fn tag(tag: &str) -> Result<(), ()> {
    assert(
        std::process::Command::new("git")
            .args(["tag", tag])
            .status()
            .unwrap()
            .success(),
        "tag failed",
    )
}

fn push_tag(tag: &str) -> Result<(), ()> {
    assert(
        std::process::Command::new("git")
            .args(["push", "origin", tag])
            .status()
            .unwrap()
            .success(),
        "push tag failed",
    )
}

fn get_last_tag() -> Option<String> {
    let cmd = std::process::Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .output()
        .unwrap();
    if cmd.status.success() {
        Some(String::from_utf8(cmd.stdout).unwrap().trim().to_owned())
    } else {
        None
    }
}
