//! Emits `BUILD_VERSION` into the binary at compile time.
//!
//! The SPA is baked into the binary (`src/web.rs` uses `include_str!` on `web/`), so there is
//! no separate static asset to version. A deployed operator otherwise has no way to tell
//! whether a new binary actually landed, which makes "did the deploy take effect?"
//! undecidable from the outside. `BUILD_VERSION` closes that gap: the SPA shows it in the
//! footer and `/api/status` exposes it, so one browser refresh answers the question.
//!
//! The UTC build timestamp is the load-bearing part. It changes on every build cargo
//! recompiles, and `date` is guaranteed present in the Debian-based build images. The git
//! SHA is best-effort: the production build image is `rust:slim-trixie`, which does not ship
//! git, so absence must not degrade the fingerprint — the fallback still marks which build
//! ran.
use std::process::Command;

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_sha() -> Option<String> {
    command_stdout("git", &["rev-parse", "--short", "HEAD"])
}

fn build_time_utc() -> String {
    command_stdout("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(|| "unknown".to_owned())
}

fn main() {
    let version = match git_sha() {
        Some(sha) => format!("{sha}+{}", build_time_utc()),
        None => format!("nogit+{}", build_time_utc()),
    };
    println!("cargo:rustc-env=BUILD_VERSION={version}");
}
