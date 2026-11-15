// The version as checkd reports it: the crate's, with the git commit it was
// built from (`0.0.0+be7dbb3`), so a running build can be told from another.
// Without git (a source tarball), CHECKD_COMMIT may give it, or it is left out.

use std::process::Command;

fn main() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.git");
    for path in ["HEAD", "refs/heads", "packed-refs"] {
        println!("cargo:rerun-if-changed={root}/{path}");
    }
    println!("cargo:rerun-if-env-changed=CHECKD_COMMIT");
    let commit = std::env::var("CHECKD_COMMIT").ok().filter(|c| !c.is_empty()).or_else(|| {
        let out = Command::new("git").args(["rev-parse", "--short=7", "HEAD"]).current_dir(env!("CARGO_MANIFEST_DIR")).output().ok()?;
        let hash = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && hash.chars().all(|c| c.is_ascii_hexdigit()) && !hash.is_empty()).then_some(hash)
    });
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let full = match commit {
        Some(c) => format!("{version}+{c}"),
        None => version,
    };
    println!("cargo:rustc-env=CHECKD_VERSION={full}");
}
