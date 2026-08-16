//! FIXTURE — deliberately unsafe. Never compiled, never run. See ../README.md.

use std::process::Command;

/// Shells out, so every interpolated value becomes shell syntax.
pub fn deploy(target: &str) {
    let script = format!("deploy.sh {target}");
    Command::new("sh").arg("-c").arg(script).status().unwrap();
}

/// Panics with no diagnostic when the variable is unset.
pub fn endpoint() -> String {
    std::env::var("ROTEIRO_ENDPOINT").unwrap()
}
