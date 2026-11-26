//! The commit the binary is built from, for the session log's first line
//! (`SPARK_GIT_HASH` overrides; "desconhecido" outside a git checkout).
//! Only this front end is rebuilt when the commit changes.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn main() {
    println!("cargo:rerun-if-env-changed=SPARK_GIT_HASH");
    let hash = std::env::var("SPARK_GIT_HASH")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| git(&["rev-parse", "--short=10", "HEAD"]))
        .unwrap_or_else(|| "desconhecido".to_owned());
    println!("cargo:rustc-env=SPARK_GIT_HASH={hash}");
    // Rebuilt when HEAD moves (a checkout, a commit); only files that
    // exist, or cargo would run this every build.
    let mut watch = vec![git(&["rev-parse", "--git-path", "HEAD"])];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(git(&["rev-parse", "--git-path", &branch]));
    }
    for path in watch.into_iter().flatten() {
        if std::path::Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
