use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Read as _,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail, ensure};
use sha2::{Digest as _, Sha256};

use crate::{cargo, execute, output};

const PACKAGER_VERSION: &str = include_str!("../packager-version.txt");
const ABOUT_VERSION: &str = include_str!("../about-version.txt");
const APP_ID: &str = "org.tgbooks.TelegramBookDownloader";
const APP_NAME: &str = "Telegram Books";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Platform {
    Macos,
    Windows,
}

impl Platform {
    fn for_host(os: &str, arch: &str) -> Result<Self> {
        match (os, arch) {
            ("macos", "aarch64") => Ok(Self::Macos),
            ("windows", "x86_64") => Ok(Self::Windows),
            _ => bail!("packaging requires native macOS Apple Silicon or Windows x64 MSVC"),
        }
    }

    fn triple(self) -> &'static str {
        match self {
            Self::Macos => "aarch64-apple-darwin",
            Self::Windows => "x86_64-pc-windows-msvc",
        }
    }

    fn family(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::Macos => "app",
            Self::Windows => "nsis",
        }
    }

    pub(crate) fn artifact_name(self, version: &str) -> String {
        let suffix = match self {
            Self::Macos => ".zip",
            Self::Windows => "-setup.exe",
        };
        format!("Telegram-Books-{version}-{}{suffix}", self.triple())
    }
}

fn tool_path(root: &Path) -> PathBuf {
    root.join("target/tools/bin")
        .join(format!("cargo-packager{}", std::env::consts::EXE_SUFFIX))
}

fn validate_tool_version(actual: &str) -> Result<()> {
    let expected = format!("cargo-packager {}", PACKAGER_VERSION.trim());
    ensure!(
        actual.trim() == expected,
        "expected {expected}, got {actual:?}"
    );
    Ok(())
}

fn check_tool(root: &Path) -> Result<PathBuf> {
    let tool = tool_path(root);
    ensure!(
        tool.is_file(),
        "install the pinned tool first: cargo xtask install-tools"
    );
    let actual = String::from_utf8(output(Command::new(&tool).arg("--version"))?)?;
    validate_tool_version(&actual)?;
    Ok(tool)
}

pub(crate) fn install_tools(root: &Path) -> Result<()> {
    for (name, version) in [
        ("cargo-packager", PACKAGER_VERSION),
        ("cargo-about", ABOUT_VERSION),
    ] {
        let mut command = cargo(root);
        command
            .args([
                "install",
                name,
                "--version",
                version.trim(),
                "--locked",
                "--root",
            ])
            .arg(root.join("target/tools"));
        if name == "cargo-about" {
            command.args(["--features", "cli"]);
        }
        execute(&mut command)?;
    }
    Ok(())
}

pub(crate) fn app_version(root: &Path) -> Result<String> {
    let metadata: serde_json::Value = serde_json::from_slice(&output(cargo(root).args([
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
        "--locked",
    ]))?)?;
    version_from_metadata(&metadata)
}

fn version_from_metadata(metadata: &serde_json::Value) -> Result<String> {
    let packages = metadata["packages"]
        .as_array()
        .context("Cargo metadata has no packages")?;
    packages
        .iter()
        .find(|package| package["name"] == "tgbooks-app")
        .and_then(|package| package["version"].as_str())
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .context("Cargo metadata has no tgbooks-app version")
}

fn build_command(root: &Path, platform: Platform) -> Command {
    let mut command = cargo(root);
    command
        .args([
            "build",
            "-p",
            "tgbooks-app",
            "--release",
            "--locked",
            "--target",
            platform.triple(),
            "--target-dir",
        ])
        .arg(root.join("target"));
    command
}

fn signing_variable(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name.starts_with("APPLE_") || name.starts_with("CARGO_PACKAGER_SIGN_")
}

fn remove_signing_variables(command: &mut Command, names: impl IntoIterator<Item = OsString>) {
    for name in names {
        if signing_variable(&name) {
            command.env_remove(name);
        }
    }
}

fn packager_command(root: &Path, tool: &Path, platform: Platform, staging: &Path) -> Command {
    let mut command = Command::new(tool);
    command
        .current_dir(root)
        .arg("--manifest-path")
        .arg(root.join("crates/app/Cargo.toml"))
        .args([
            "--packages",
            "tgbooks-app",
            "--release",
            "--target",
            platform.triple(),
        ])
        .arg("--binaries-dir")
        .arg(root.join("target").join(platform.triple()).join("release"))
        .arg("--out-dir")
        .arg(staging)
        .args(["--formats", platform.format()]);
    // Test packaging must not notarize or use an inherited private signing key.
    // Only variable names are inspected; secret values are never logged.
    remove_signing_variables(&mut command, std::env::vars_os().map(|(name, _)| name));
    command
}

fn verify_bundle(bundle: &Path, version: &str) -> Result<()> {
    let contents = bundle.join("Contents");
    let info = plist::Value::from_file(contents.join("Info.plist"))?
        .into_dictionary()
        .context("bundle Info.plist is not a dictionary")?;
    for (key, value) in [
        ("CFBundleIdentifier", APP_ID),
        ("CFBundleName", APP_NAME),
        ("CFBundleExecutable", "tgbooks"),
        ("CFBundleShortVersionString", version),
        ("LSMinimumSystemVersion", "12.0"),
    ] {
        ensure!(
            info.get(key).and_then(plist::Value::as_string) == Some(value),
            "bundle has unexpected {key}"
        );
    }
    ensure!(
        info.get("TgbooksOfferInstallation")
            .and_then(plist::Value::as_boolean)
            == Some(true),
        "bundle installation marker is missing"
    );
    ensure!(
        contents.join("MacOS/tgbooks").is_file(),
        "bundle executable is missing"
    );
    let icon = info
        .get("CFBundleIconFile")
        .and_then(plist::Value::as_string)
        .context("bundle icon is missing")?;
    let mut components = Path::new(icon).components();
    ensure!(
        matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none()
            && contents.join("Resources").join(icon).is_file(),
        "bundle icon is missing or invalid"
    );
    for name in [
        "LICENSE",
        "NOTICE",
        "LICENSE-APACHE",
        "THIRD_PARTY_NOTICES.txt",
    ] {
        ensure!(
            contents.join("Resources/licenses").join(name).is_file(),
            "bundle notice is missing: {name}"
        );
    }
    Ok(())
}

fn find_installer(staging: &Path) -> Result<PathBuf> {
    let mut installers = Vec::new();
    for entry in fs::read_dir(staging)? {
        let path = entry?.path();
        if path.is_file() && path.extension() == Some(OsStr::new("exe")) {
            installers.push(path);
        }
    }
    ensure!(installers.len() == 1, "expected exactly one NSIS installer");
    installers.pop().context("NSIS installer is missing")
}

fn publish_artifact(artifact: &Path, destination: &Path) -> Result<()> {
    // Both paths are inside target/: replace the archive atomically, never leave
    // a half-copied package or overwrite an extracted/running application.
    fs::rename(artifact, destination).context("could not publish local package")?;
    let digest = checksum(destination)?;
    let name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .context("invalid package filename")?;
    fs::write(
        destination.with_file_name(format!("{name}.sha256")),
        format!("{digest}  {name}\n"),
    )?;
    Ok(())
}

pub(crate) fn checksum(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn generate_notices(root: &Path, platform: Platform) -> Result<()> {
    let tool = root
        .join("target/tools/bin")
        .join(format!("cargo-about{}", std::env::consts::EXE_SUFFIX));
    ensure!(
        tool.is_file(),
        "install the license tool first: cargo xtask install-tools"
    );
    let version = String::from_utf8(output(Command::new(&tool).arg("--version"))?)?;
    ensure!(
        version.trim() == format!("cargo-about {}", ABOUT_VERSION.trim()),
        "unexpected cargo-about version"
    );
    let directory = root.join("target/licenses");
    fs::create_dir_all(&directory)?;
    let notices = directory.join("THIRD_PARTY_NOTICES.txt");
    execute(
        Command::new(tool)
            .current_dir(root)
            .args(["generate", "--locked", "--fail", "--manifest-path"])
            .arg(root.join("crates/app/Cargo.toml"))
            .args(["--target", platform.triple(), "--config"])
            .arg(root.join("licenses/about.toml"))
            .arg("--output-file")
            .arg(&notices)
            .arg(root.join("licenses/third-party.hbs")),
    )?;
    ensure!(
        fs::metadata(notices)?.len() > 0,
        "license notices are empty"
    );
    Ok(())
}

fn macos_architecture_command(binary: &Path) -> Command {
    let mut command = Command::new("/usr/bin/lipo");
    // -verify_arch consumes a list of architectures in Xcode 16. Put the input
    // first so it cannot be parsed as another architecture (Xcode 27 tolerates
    // the old order, which hid this incompatibility in local validation).
    command.arg(binary).args(["-verify_arch", "arm64"]);
    command
}

pub(crate) fn package(root: &Path) -> Result<()> {
    let platform = Platform::for_host(std::env::consts::OS, std::env::consts::ARCH)?;
    let tool = check_tool(root)?;
    let version = app_version(root)?;
    generate_notices(root, platform)?;
    execute(&mut build_command(root, platform))?;
    let staging_root = root.join("target/packaging");
    fs::create_dir_all(&staging_root)?;
    let destination = root.join("target/packages").join(platform.family());
    fs::create_dir_all(&destination)?;
    // cargo-packager removes a previous .app before building. Always stage in a
    // new owned directory, never in target/dev-bundle or a user's Applications.
    let staging = tempfile::Builder::new()
        .prefix("build-")
        .tempdir_in(staging_root)?;
    execute(&mut packager_command(root, &tool, platform, staging.path()))?;
    let name = platform.artifact_name(&version);
    let artifact = staging.path().join(&name);
    match platform {
        Platform::Macos => {
            let bundle = staging.path().join(format!("{APP_NAME}.app"));
            verify_bundle(&bundle, &version)?;
            execute(&mut macos_architecture_command(
                &bundle.join("Contents/MacOS/tgbooks"),
            ))?;
            execute(
                Command::new("/usr/bin/codesign")
                    .args(["--force", "--sign", "-"])
                    .arg(&bundle),
            )?;
            execute(
                Command::new("/usr/bin/codesign")
                    .args(["--verify", "--deep", "--strict"])
                    .arg(&bundle),
            )?;
            // ditto preserves executable permissions and .app symlinks.
            execute(
                Command::new("/usr/bin/ditto")
                    .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
                    .arg(bundle)
                    .arg(&artifact),
            )?;
        }
        Platform::Windows => {
            fs::rename(find_installer(staging.path())?, &artifact)?;
        }
    }
    let final_path = destination.join(name);
    publish_artifact(&artifact, &final_path)?;
    println!("Test package: {}", final_path.display());
    println!("Not notarized / not signed by a trusted publisher. No release was published.");
    Ok(())
}

#[cfg(test)]
#[path = "package_tests.rs"]
mod tests;
