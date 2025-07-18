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
use std::{path::Path, str::FromStr};

use clap::Parser;
use regex::{Captures, Regex};
use semver::Version;

fn main() {
    let cli = Cli::parse();
    cli.boop();
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

impl Cli {
    #[expect(clippy::too_many_lines)]
    fn boop(&self) {
        assert_git_clean();
        let re = Regex::new("((VERSION|version) ?= ?)\"([^\"]+)\"").unwrap();
        let files = ["Cargo.toml", ".env"];
        let (matching_files, versions): (Vec<&'static Path>, Vec<String>) = files
            .into_iter()
            .map(Path::new)
            .filter_map(|file| {
                let contents = std::fs::read_to_string(file).ok()?;
                let cap = re.captures(&contents)?;
                Some((file, cap.get(3)?.as_str().to_owned()))
            })
            .unzip();
        assert!(!versions.is_empty(), "no versions found");
        assert!(
            all_equal(&versions),
            "no consistent version found: {versions:?}"
        );
        let from_version = semver::Version::parse(&versions[0]).unwrap();
        let last_tag = get_last_tag();
        if let Some(last_tag) = &last_tag {
            let stripped_last_tag = last_tag.strip_prefix('v').unwrap_or(last_tag);
            if !stripped_last_tag.is_empty()
                && from_version.pre.is_empty()
                && from_version != semver::Version::parse(stripped_last_tag).unwrap()
            {
                panic!("last git tag does not match the detected tag");
            }
        }
        assert!(from_version.build.is_empty(), "build suffix unsupported");
        let to_version = self.increment.increment(&from_version);
        let to_version_tag = last_tag
            .map(|last_tag| {
                if last_tag.starts_with('v') {
                    format!("v{to_version}")
                } else {
                    to_version.to_string()
                }
            })
            .unwrap_or_else(|| format!("v{to_version}"));

        eprintln!("Upgrading version {from_version} to {to_version}");
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
        let ops_display;
        if let Some(last) = ops.pop() {
            ops_display = ops.iter().fold(String::new(), |mut output, x| {
                let _ = write!(output, ", {x}");
                output
            }) + " and "
                + last;
        } else {
            ops_display = String::new();
        }
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
            return;
        }

        for file in matching_files {
            let contents = std::fs::read_to_string(file).unwrap();
            let replaced_contents = re.replace(&contents, |caps: &Captures| {
                format!("{}\"{}\"", &caps[1], to_version)
            });
            std::fs::write(file, replaced_contents.as_ref()).unwrap();
        }

        cargo_check();
        eprintln!("Upgraded!");

        if self.commit {
            let msg = format!("Version {to_version}");
            commit(&msg);
            if self.push {
                push();
            }

            if self.tag {
                tag(&to_version_tag);
                if self.push {
                    push_tag(&to_version_tag);
                }
            }
        } else {
            if self.tag {
                eprintln!("Can't tag when -c / --commit is not enabled");
            }
            if self.push {
                eprintln!("Can't push when -c / --commit is not enabled");
            }
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

fn cargo_check() {
    assert!(
        std::process::Command::new("cargo")
            .args(["check", "-q"])
            .status()
            .unwrap()
            .success(),
        "cargo check failed"
    );
}

fn assert_git_clean() {
    assert!(
        std::process::Command::new("git")
            .args(["diff", "--cached", "--exit-code"])
            .status()
            .unwrap()
            .success(),
        "uncommitted changes",
    );
}

fn commit(message: &str) {
    assert!(
        std::process::Command::new("git")
            .args(["commit", "-am", message])
            .status()
            .unwrap()
            .success(),
        "commit failed"
    );
}

fn push() {
    assert!(
        std::process::Command::new("git")
            .args(["push"])
            .status()
            .unwrap()
            .success(),
        "push failed"
    );
}

fn tag(tag: &str) {
    assert!(
        std::process::Command::new("git")
            .args(["tag", tag])
            .status()
            .unwrap()
            .success(),
        "tag failed"
    );
}

fn push_tag(tag: &str) {
    assert!(
        std::process::Command::new("git")
            .args(["push", "origin", tag])
            .status()
            .unwrap()
            .success(),
        "push tag failed"
    );
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
