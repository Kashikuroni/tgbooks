use super::*;

fn locations(root: &Path) -> Locations {
    Locations {
        system: root.join("Applications"),
        user: Some(root.join("User/Applications")),
    }
}

fn bundle(path: &Path, enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(path.join("Contents/MacOS"))?;
    fs::copy("/usr/bin/true", path.join("Contents/MacOS/tgbooks"))?;
    let mut info = plist::Dictionary::new();
    for (key, value) in [
        ("CFBundleIdentifier", APP_ID),
        ("CFBundleExecutable", "tgbooks"),
        ("CFBundleName", APP_NAME),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleVersion", "1"),
        ("CFBundleShortVersionString", "0.1.0"),
    ] {
        info.insert(key.into(), value.into());
    }
    info.insert(INSTALL_KEY.into(), enabled.into());
    plist::Value::Dictionary(info).to_file_xml(path.join("Contents/Info.plist"))?;
    Ok(())
}

fn signed_bundle(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    bundle(path, true)?;
    let status = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .output()?;
    if !status.status.success() {
        return Err("could not sign test fixture".into());
    }
    Ok(())
}

#[test]
fn only_packaged_release_bundles_outside_both_applications_roots_are_offered()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let roots = locations(root.path());
    let downloaded = root.path().join("Downloads/Telegram Books.app");
    bundle(&downloaded, true)?;
    assert!(proposal(&downloaded, roots.clone(), true).is_some());
    assert!(proposal(&downloaded, roots.clone(), false).is_none());
    bundle(&downloaded, false)?;
    assert!(proposal(&downloaded, roots.clone(), true).is_none());
    for path in [
        roots.system.join("Telegram Books.app"),
        roots.system.join("Tools/Telegram Books.app"),
        roots
            .user
            .as_ref()
            .ok_or("missing user directory")?
            .join("Telegram Books.app"),
    ] {
        bundle(&path, true)?;
        assert!(proposal(&path, roots.clone(), true).is_none());
    }
    Ok(())
}

#[test]
fn similar_names_are_not_applications_and_symlinks_are_resolved()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let roots = locations(root.path());
    let similar = root.path().join("Applications backup/Telegram Books.app");
    bundle(&similar, true)?;
    assert!(proposal(&similar, roots.clone(), true).is_some());
    let installed = roots.system.join("Telegram Books.app");
    bundle(&installed, true)?;
    let alias = root.path().join("Telegram Books.app");
    std::os::unix::fs::symlink(installed, &alias)?;
    assert!(proposal(&alias, roots, true).is_none());
    Ok(())
}

#[test]
fn invalid_foreign_or_escaping_bundles_are_never_install_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Telegram Books.app");
    bundle(&source, true)?;
    let executable = source.join("Contents/MacOS/tgbooks");
    fs::remove_file(&executable)?;
    std::os::unix::fs::symlink("/usr/bin/true", &executable)?;
    assert!(proposal(&source, locations(root.path()), true).is_none());
    fs::remove_file(executable)?;
    bundle(&source, true)?;
    let plist = source.join("Contents/Info.plist");
    let mut info = plist::Value::from_file(&plist)?
        .into_dictionary()
        .ok_or("not a dictionary")?;
    info.insert("CFBundleIdentifier".into(), "other.application".into());
    plist::Value::Dictionary(info).to_file_xml(plist)?;
    assert!(proposal(&source, locations(root.path()), true).is_none());
    assert!(
        proposal(
            &root.path().join("missing.app"),
            locations(root.path()),
            true
        )
        .is_none()
    );
    Ok(())
}

#[test]
fn installation_preserves_source_signature_and_does_not_offer_again()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Downloads/Книги ' Books.app");
    signed_bundle(&source)?;
    let quarantine = "0081;6722346f;TelegramBooksTest;";
    assert!(
        Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", quarantine])
            .arg(&source)
            .output()?
            .status
            .success()
    );
    let roots = locations(root.path());
    let plan = proposal(&source, roots.clone(), true).ok_or("missing proposal")?;
    let installed = install(plan, Destination::User).map_err(|_| "installation failed")?;
    assert!(
        source.join("Contents/MacOS/tgbooks").exists(),
        "never delete the source"
    );
    assert!(
        installed.starts_with(
            roots
                .user
                .as_ref()
                .ok_or("missing user directory")?
                .canonicalize()?
        )
    );
    assert!(proposal(&installed, roots, true).is_none());
    let attribute = Command::new("/usr/bin/xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(&installed)
        .output()?;
    assert!(attribute.status.success());
    // macOS may normalize the timestamp/agent fields during a copy. Preserve
    // the quarantine restriction, not the exact spelling of its metadata.
    let attribute = String::from_utf8(attribute.stdout)?;
    let flags = attribute
        .trim()
        .split(';')
        .next()
        .ok_or("missing quarantine flags")?;
    assert_ne!(u32::from_str_radix(flags, 16)? & 1, 0);
    assert!(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(installed)
            .output()?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn existing_app_symlink_and_empty_directory_are_not_overwritten()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Downloads/Telegram Books.app");
    signed_bundle(&source)?;
    let roots = locations(root.path());
    fs::create_dir_all(&roots.system)?;
    let installed = roots.system.join("Telegram Books.app");
    fs::create_dir(&installed)?;
    let plan = proposal(&source, roots.clone(), true).ok_or("missing proposal")?;
    assert_eq!(
        install(plan, Destination::System),
        Err(InstallError::Occupied)
    );
    assert!(installed.is_dir());
    fs::remove_dir(&installed)?;
    let sentinel = root.path().join("sentinel");
    fs::write(&sentinel, b"keep")?;
    std::os::unix::fs::symlink(&sentinel, &installed)?;
    let plan = proposal(&source, roots, true).ok_or("missing proposal")?;
    assert_eq!(
        install(plan, Destination::System),
        Err(InstallError::Occupied)
    );
    assert_eq!(fs::read(sentinel)?, b"keep");
    Ok(())
}

#[test]
fn inaccessible_destination_leaves_source_and_existing_files_intact()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Downloads/Telegram Books.app");
    bundle(&source, true)?;
    let roots = locations(root.path());
    fs::write(&roots.system, b"not a directory")?;
    let plan = proposal(&source, roots.clone(), true).ok_or("missing proposal")?;
    assert_eq!(
        install(plan, Destination::System),
        Err(InstallError::Permission)
    );
    assert!(source.join("Contents/MacOS/tgbooks").exists());
    assert_eq!(fs::read(roots.system)?, b"not a directory");
    Ok(())
}

#[test]
fn a_racing_destination_is_preserved_at_the_atomic_publish_step()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("staged.app");
    let destination = root.path().join("installed.app");
    fs::create_dir(&source)?;
    fs::write(source.join("new"), b"new")?;
    fs::create_dir(&destination)?;
    assert_eq!(publish(&source, &destination), Err(InstallError::Occupied));
    assert!(source.join("new").exists());
    assert_eq!(fs::read_dir(destination)?.count(), 0);
    Ok(())
}

#[test]
fn failed_signature_check_cleans_only_the_owned_staging_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Downloads/Telegram Books.app");
    bundle(&source, true)?; // Not signed as a bundle.
    let roots = locations(root.path());
    let plan = proposal(&source, roots.clone(), true).ok_or("missing proposal")?;
    assert_eq!(
        install(plan, Destination::System),
        Err(InstallError::Signature)
    );
    assert!(source.exists());
    assert_eq!(fs::read_dir(roots.system)?.count(), 0);
    Ok(())
}
