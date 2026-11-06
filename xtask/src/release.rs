//! Release validation only. GitHub publication is isolated to the workflow.
use std::{collections::BTreeSet, fs, path::Path};

use anyhow::{Context as _, Result, ensure};

use crate::package::{Platform, app_version, checksum};

fn validate_tag(tag: &str, version: &str) -> Result<()> {
    // Cargo validates the version as SemVer. Exact equality also rejects path
    // separators, shell text, alternative spellings and a missing "v" prefix.
    ensure!(
        tag == format!("v{version}"),
        "release tag must be v{version}, got {tag:?}"
    );
    Ok(())
}

pub(crate) fn check(root: &Path, tag: &str) -> Result<String> {
    let version = app_version(root)?;
    validate_tag(tag, &version)?;
    let notes = root.join("release-notes").join(format!("{tag}.md"));
    ensure!(
        notes.is_file() && fs::metadata(&notes)?.len() > 0,
        "release notes are missing: {}",
        notes.display()
    );
    println!("Validated release tag {tag}");
    Ok(version)
}

fn verify_assets(directory: &Path, version: &str) -> Result<()> {
    let packages = [
        Platform::Macos.artifact_name(version),
        Platform::Windows.artifact_name(version),
    ];
    let expected: BTreeSet<_> = packages
        .iter()
        .flat_map(|name| [name.clone(), format!("{name}.sha256")])
        .collect();
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "release assets must be regular files, not directories or symlinks"
        );
        actual.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid asset filename"))?,
        );
    }
    ensure!(
        actual == expected,
        "expected exactly the two platform packages and their checksums; got {actual:?}"
    );
    for name in packages {
        let path = directory.join(&name);
        ensure!(
            fs::metadata(&path)?.len() > 0,
            "empty release package: {name}"
        );
        let text = fs::read_to_string(directory.join(format!("{name}.sha256")))?;
        let parts: Vec<_> = text.split_whitespace().collect();
        ensure!(
            parts.len() == 2 && parts[1] == name,
            "checksum must name exactly {name}"
        );
        ensure!(
            parts[0].len() == 64 && parts[0].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid SHA-256 for {name}"
        );
        ensure!(
            checksum(&path)?.eq_ignore_ascii_case(parts[0]),
            "checksum mismatch for {name}"
        );
    }
    Ok(())
}

pub(crate) fn verify(root: &Path, tag: &str, directory: &Path) -> Result<()> {
    let version = check(root, tag)?;
    verify_assets(directory, &version).context("release artifacts failed validation")?;
    println!("Both platform packages and SHA-256 files are valid");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(directory: &Path) -> Result<()> {
        for platform in [Platform::Macos, Platform::Windows] {
            let name = platform.artifact_name("0.1.0");
            fs::write(directory.join(&name), b"test package")?;
            fs::write(
                directory.join(format!("{name}.sha256")),
                format!("{}  {name}\n", checksum(&directory.join(&name))?),
            )?;
        }
        Ok(())
    }

    #[test]
    fn tags_must_match_the_cargo_version_exactly() -> Result<()> {
        validate_tag("v0.1.0", "0.1.0")?;
        for tag in [
            "0.1.0",
            "v0.1.1",
            "v01.1.0",
            "../v0.1.0",
            "v0.1.0; echo unsafe",
        ] {
            assert!(validate_tag(tag, "0.1.0").is_err());
        }
        Ok(())
    }

    #[test]
    fn both_packages_are_required_and_their_contents_must_match_checksums() -> Result<()> {
        let root = tempfile::tempdir()?;
        assets(root.path())?;
        verify_assets(root.path(), "0.1.0")?;
        let windows = root.path().join(Platform::Windows.artifact_name("0.1.0"));
        fs::remove_file(&windows)?;
        assert!(verify_assets(root.path(), "0.1.0").is_err());
        assets(root.path())?;
        fs::write(windows, b"corrupt")?;
        assert!(verify_assets(root.path(), "0.1.0").is_err());
        Ok(())
    }

    #[test]
    fn unexpected_files_empty_packages_and_checksum_path_traversal_are_rejected() -> Result<()> {
        let root = tempfile::tempdir()?;
        assets(root.path())?;
        let extra = root.path().join("private.session");
        fs::write(&extra, b"not for publication")?;
        assert!(verify_assets(root.path(), "0.1.0").is_err());
        fs::remove_file(extra)?;
        let name = Platform::Macos.artifact_name("0.1.0");
        fs::write(
            root.path().join(format!("{name}.sha256")),
            format!("{}  ../{name}\n", "0".repeat(64)),
        )?;
        assert!(verify_assets(root.path(), "0.1.0").is_err());
        assets(root.path())?;
        fs::write(root.path().join(name), b"")?;
        assert!(verify_assets(root.path(), "0.1.0").is_err());
        Ok(())
    }
}
