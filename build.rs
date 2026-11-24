//! Puts the build time and the git revision into the binary (`jff --version`).

use std::process::Command;

/// The output of a command, trimmed, `None` when it failed or printed nothing.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn main() {
    // No `rerun-if-changed`: cargo then runs this whenever a file of the package changed, so the time is that of the last build that had something new in it.
    let built = run("date", &["+%Y-%m-%d %H:%M:%S %z"]).unwrap_or_else(|| "unknown time".into());
    let rev = run("git", &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "no-git".into());
    let dirty = run("git", &["status", "--porcelain"]).is_some();
    println!("cargo:rustc-env=JFF_BUILT={built}");
    println!(
        "cargo:rustc-env=JFF_REV={rev}{}",
        if dirty { "-dirty" } else { "" }
    );
}
