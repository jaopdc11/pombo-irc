//! Embeds the git commit this build comes from as `POMBO_VERSION`.
//! Client and relay must carry the same value to talk to each other.

use std::path::Path;
use std::process::Command;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let mut version = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    if git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty()) {
        version.push_str("-dirty");
    }
    println!("cargo:rustc-env=POMBO_VERSION={version}");
    let dot_git = root.join(".git");
    for file in ["HEAD", "index", "packed-refs"] {
        println!("cargo:rerun-if-changed={}", dot_git.join(file).display());
    }
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        println!(
            "cargo:rerun-if-changed={}",
            dot_git.join(head_ref).display()
        );
    }
}
