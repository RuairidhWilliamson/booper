#![warn(missing_docs)]

use std::{path::Path, str::FromStr};

use clap::Parser;
use regex::{Captures, Regex};
use semver::Version;

fn main() {
    let cli = Cli::parse();
    cli.boop();
}

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    /// Can be one of `patch`, `minor`, `major` or an exact version e.g. `1.0.3`
    #[arg(default_value = "patch")]
    increment: VersionIncrement,
    #[arg(short, long)]
    commit: bool,
    #[arg(short, long)]
    tag: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionIncrement {
    Patch,
    Minor,
    Major,
    Exact(Version),
}

impl FromStr for VersionIncrement {
    type Err = semver::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_ref() {
            "patch" => Self::Patch,
            "minor" => Self::Minor,
            "major" => Self::Major,
            _ => Self::Exact(Version::from_str(s)?),
        })
    }
}

impl VersionIncrement {
    fn increment(&self, current: &Version) -> Version {
        match self {
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
            Self::Exact(version) => version.clone(),
        }
    }
}

impl Cli {
    fn boop(&self) {
        if !check_git_clean() {
            panic!("Uncommitted git changes");
        }
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
        if versions.is_empty() {
            panic!("no versions found");
        }
        if !all_equal(&versions) {
            panic!("no consistent version found: {versions:?}");
        }
        let from_version = semver::Version::parse(&versions[0]).unwrap();
        let to_version = self.increment.increment(&from_version);

        println!("Upgrading version {from_version} to {to_version}");
        println!("The following files will be changed:");
        for file in &matching_files {
            println!("\t{}", file.display());
        }
        if !dialoguer::Confirm::new()
            .with_prompt("Do you want to continue?")
            .interact()
            .unwrap()
        {
            return;
        }

        matching_files.into_iter().for_each(|file| {
            let contents = std::fs::read_to_string(file).unwrap();
            let replaced_contents = re.replace(&contents, |caps: &Captures| {
                format!("{}\"{}\"", &caps[1], to_version)
            });
            std::fs::write(file, replaced_contents.as_ref()).unwrap();
        });

        cargo_check();
        println!("Upgraded!");

        if self.commit {
            let v = to_version.to_string();
            let msg = format!("Version {}", v);
            commit(&msg);
            push();

            if self.tag {
                tag(&v);
                push_tags();
            }
        }
    }
}

fn all_equal(v: &[String]) -> bool {
    let first = v.get(0);
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
    assert!(std::process::Command::new("cargo")
        .args(["check", "-q"])
        .status()
        .unwrap()
        .success());
}

fn check_git_clean() -> bool {
    std::process::Command::new("git")
        .args(["diff", "--cached", "--exit-code"])
        .status()
        .unwrap()
        .success()
}

fn commit(message: &str) {
    assert!(std::process::Command::new("git")
        .args(["commit", "-am", message])
        .status()
        .unwrap()
        .success());
}

fn push() {
    assert!(std::process::Command::new("git")
        .args(["push"])
        .status()
        .unwrap()
        .success());
}

fn tag(tag: &str) {
    assert!(std::process::Command::new("git")
        .args(["tag", tag])
        .status()
        .unwrap()
        .success());
}

fn push_tags() {
    assert!(std::process::Command::new("git")
        .args(["push", "--tags"])
        .status()
        .unwrap()
        .success());
}
