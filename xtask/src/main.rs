//! Check and packaging entry points for disktree.
//!
//! Each gate is reachable on its own (`cargo xtask fmt`) and through the
//! aggregate CI runs (`cargo xtask lint`), mirroring how omatrack exposes every
//! linter both as a build target and as a labelled test. `lint` never fixes
//! anything: it reports the diff and fails, so a red local run is the same
//! signal CI gives.

mod bundle;

use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  lint      fmt --check, then clippy over the workspace with -D warnings
  fmt       rustfmt --check over the workspace
  fmt-fix   rustfmt in place
  clippy    clippy --workspace --all-targets -- -D warnings
  test      cargo test --workspace, then the web shim's node --test suite
  ci        lint, then test
  bundle    macOS: target/bundle/disktree.app and its zip
              --sign IDENTITY   sign with a Developer ID (default: ad hoc)
              --notarize        notarize and staple; needs NOTARY_PROFILE, or
                                NOTARY_KEY, NOTARY_KEY_ID and NOTARY_ISSUER
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().map_or("lint", String::as_str);

    let outcome = match task {
        "lint" => fmt(false).and_then(|()| clippy()),
        "fmt" => fmt(false),
        "fmt-fix" => fmt(true),
        "clippy" => clippy(),
        "test" => test(),
        "ci" => fmt(false).and_then(|()| clippy()).and_then(|()| test()),
        "bundle" => bundle::bundle(&args[1..]),
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("unknown task: {other}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask {task}: {message}");
            ExitCode::FAILURE
        }
    }
}

fn fmt(fix: bool) -> Result<(), String> {
    let mut args = vec!["fmt", "--all"];
    args.push(if fix { "" } else { "--check" });
    let args: Vec<&str> = args.into_iter().filter(|a| !a.is_empty()).collect();
    run(&args)
}

fn clippy() -> Result<(), String> {
    run(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ])
}

fn test() -> Result<(), String> {
    run(&["test", "--workspace"])?;
    web_js()
}

/// The browser shim's suite (`crates/disktree-web/tests-js`), through the
/// Node every GitHub runner already has. Without Node it is skipped with a
/// note rather than failed: the Rust gates carry the crate.
fn web_js() -> Result<(), String> {
    if Command::new("node")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        println!("xtask test: no node on PATH; skipping the shim suite");
        return Ok(());
    }
    // A glob, not a bare directory: node treats the latter as an entry
    // point to load. There is no shell here, so node expands the pattern.
    let status = Command::new("node")
        .args(["--test", "crates/disktree-web/tests-js/*.test.mjs"])
        .stdin(Stdio::null())
        .status()
        .map_err(|err| format!("could not start `node --test`: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("`node --test crates/disktree-web/tests-js/` failed".to_string())
    }
}

fn run(args: &[&str]) -> Result<(), String> {
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let display = format!("{cargo} {}", args.join(" "));
    let status = Command::new(cargo)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|err| format!("could not start `{display}`: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{display}` failed"))
    }
}
