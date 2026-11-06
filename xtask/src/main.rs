//! Project-local Rust tooling. Never starts the app backend or publishes a release.
mod icons;
mod package;
mod release;

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Context as _, Result, bail, ensure};

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<std::ffi::OsString>) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask must be inside the workspace")?;
    match arguments.first().and_then(|argument| argument.to_str()) {
        None | Some("--help" | "-h" | "help") if arguments.len() <= 1 => {
            println!(
                "Usage:\n  cargo xtask install-tools\n  cargo xtask package\n  cargo xtask icons [--output DIR]\n  cargo xtask check-release TAG\n  cargo xtask verify-release TAG DIR"
            );
        }
        Some("install-tools") if arguments.len() == 1 => package::install_tools(root)?,
        Some("package") if arguments.len() == 1 => package::package(root)?,
        Some("check-release") if arguments.len() == 2 => {
            release::check(root, arguments[1].to_str().context("tag must be UTF-8")?)?;
        }
        Some("verify-release") if arguments.len() == 3 => {
            release::verify(
                root,
                arguments[1].to_str().context("tag must be UTF-8")?,
                &root.join(PathBuf::from(&arguments[2])),
            )?;
        }
        Some("icons") => {
            let output = match arguments.as_slice() {
                [_] => root.join("crates/app/resources/branding"),
                [_, flag, path] if flag == "--output" => root.join(PathBuf::from(path)),
                _ => bail!("usage: cargo xtask icons [--output DIR]"),
            };
            icons::generate(root, &output)?;
        }
        _ => bail!("unknown task or arguments; use cargo xtask --help"),
    }
    Ok(())
}

fn cargo(root: &Path) -> Command {
    // Use the same toolchain as the outer Cargo invocation, not an arbitrary
    // binary earlier on PATH. No shell interpolation of paths or arguments.
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root);
    command
}

fn execute(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("could not run {}", command.get_program().to_string_lossy()))?;
    ensure!(
        status.success(),
        "{} failed ({status})",
        command.get_program().to_string_lossy()
    );
    Ok(())
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let result = command
        .output()
        .with_context(|| format!("could not run {}", command.get_program().to_string_lossy()))?;
    ensure!(
        result.status.success(),
        "{} failed ({}): {}",
        command.get_program().to_string_lossy(),
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}
