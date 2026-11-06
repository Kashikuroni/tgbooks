use super::*;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
}

fn manifest() -> Result<toml::Table> {
    Ok(fs::read_to_string(root().join("crates/app/Cargo.toml"))?.parse()?)
}

#[test]
fn only_native_supported_architectures_are_accepted() -> Result<()> {
    assert_eq!(Platform::for_host("macos", "aarch64")?, Platform::Macos);
    assert_eq!(Platform::for_host("windows", "x86_64")?, Platform::Windows);
    for (os, arch) in [
        ("linux", "x86_64"),
        ("macos", "x86_64"),
        ("windows", "aarch64"),
    ] {
        assert!(Platform::for_host(os, arch).is_err());
    }
    assert_eq!(
        Platform::Macos.artifact_name("0.1.0"),
        "Telegram-Books-0.1.0-aarch64-apple-darwin.zip"
    );
    assert_eq!(
        Platform::Windows.artifact_name("0.1.0"),
        "Telegram-Books-0.1.0-x86_64-pc-windows-msvc-setup.exe"
    );
    Ok(())
}

#[test]
fn signing_credentials_are_removed_without_reading_or_mutating_global_values() {
    let mut command = Command::new("unused");
    remove_signing_variables(
        &mut command,
        [
            "PATH",
            "APPLE_KEYCHAIN_PROFILE",
            "CARGO_PACKAGER_SIGN_PRIVATE_KEY",
            "CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD",
            "Apple_Id",
        ]
        .into_iter()
        .map(OsString::from),
    );
    let removed: Vec<_> = command
        .get_envs()
        .map(|(name, value)| {
            assert!(value.is_none());
            name.to_string_lossy().into_owned()
        })
        .collect();
    assert_eq!(removed.len(), 4);
    assert!(!removed.iter().any(|name| name == "PATH"));
}

#[test]
fn release_and_packager_commands_preserve_arguments_with_spaces() -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("xtask books ' 世界 ")
        .tempdir()?;
    for platform in [Platform::Macos, Platform::Windows] {
        let build = build_command(root.path(), platform);
        let args: Vec<_> = build.get_args().collect();
        assert!(args.contains(&OsStr::new("--release")));
        assert!(args.contains(&OsStr::new("--locked")));
        assert!(args.contains(&OsStr::new(platform.triple())));
        assert!(!args.contains(&OsStr::new("--all-features")));
        let staging = root.path().join("new package");
        let command = packager_command(root.path(), Path::new("unused"), platform, &staging);
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&staging.as_os_str()));
        assert!(args.contains(&OsStr::new(platform.format())));
        assert!(args.contains(&root.path().join("crates/app/Cargo.toml").as_os_str()));
        assert!(!args.contains(&root.path().join("target/dev-bundle").as_os_str()));
    }
    Ok(())
}

#[test]
fn metadata_keeps_identity_and_derives_version_from_cargo() -> Result<()> {
    let manifest = manifest()?;
    let config = &manifest["package"]["metadata"]["packager"];
    assert_eq!(config["identifier"].as_str(), Some(APP_ID));
    assert_eq!(config["product-name"].as_str(), Some(APP_NAME));
    assert!(config.get("version").is_none());
    assert!(config["macos"].get("signing-identity").is_none());
    assert_eq!(
        config["nsis"]["installer-mode"].as_str(),
        Some("currentUser")
    );
    assert!(config["nsis"].get("appdata-paths").is_none());
    let platform = fs::read_to_string(root().join("crates/app/src/platform.rs"))?;
    assert!(platform.contains(&format!("APP_ID: &str = \"{APP_ID}\"")));
    assert!(platform.contains(&format!("APP_NAME: &str = \"{APP_NAME}\"")));
    let metadata = serde_json::json!({"packages": [{"name": "xtask", "version": "9.9.9"},
        {"name": "tgbooks-app", "version": "0.2.3"}]});
    assert_eq!(version_from_metadata(&metadata)?, "0.2.3");
    assert!(version_from_metadata(&serde_json::json!({"packages": []})).is_err());
    Ok(())
}

#[test]
fn resources_are_an_explicit_source_only_allowlist() -> Result<()> {
    let manifest = manifest()?;
    let resources = manifest["package"]["metadata"]["packager"]["resources"]
        .as_array()
        .context("missing resources")?;
    let mut targets = Vec::new();
    for entry in resources {
        let source = entry["src"].as_str().context("missing source")?;
        assert!(!source.contains('*'));
        if source == "../../target/licenses/THIRD_PARTY_NOTICES.txt" {
            assert_eq!(
                entry["target"].as_str(),
                Some("licenses/THIRD_PARTY_NOTICES.txt")
            );
            targets.push("licenses/THIRD_PARTY_NOTICES.txt");
            continue;
        }
        let path = root().join("crates/app").join(source).canonicalize()?;
        assert!(path.is_file() && path.starts_with(root().canonicalize()?));
        targets.push(entry["target"].as_str().context("missing destination")?);
    }
    targets.sort();
    assert_eq!(
        targets,
        [
            "licenses/LICENSE",
            "licenses/LICENSE-APACHE",
            "licenses/NOTICE",
            "licenses/THIRD_PARTY_NOTICES.txt",
        ]
    );
    Ok(())
}

#[test]
fn packaging_plist_cannot_override_identity_icon_or_version() -> Result<()> {
    let info = plist::Value::from_file(root().join("crates/app/resources/macos/Bundle.plist"))?
        .into_dictionary()
        .context("not a plist dictionary")?;
    assert_eq!(info.len(), 2);
    for key in ["NSHighResolutionCapable", "TgbooksOfferInstallation"] {
        assert_eq!(info.get(key).and_then(plist::Value::as_boolean), Some(true));
    }
    Ok(())
}

#[test]
fn windows_manifest_is_unprivileged_and_dpi_aware() -> Result<()> {
    let text = fs::read_to_string(root().join("crates/app/resources/windows/tgbooks.manifest"))?;
    let document = roxmltree::Document::parse(&text)?;
    let level = document
        .descendants()
        .find(|node| {
            node.has_tag_name((
                "urn:schemas-microsoft-com:asm.v3",
                "requestedExecutionLevel",
            ))
        })
        .context("missing execution level")?;
    assert_eq!(level.attribute("level"), Some("asInvoker"));
    assert_eq!(level.attribute("uiAccess"), Some("false"));
    for (tag, value) in [("dpiAwareness", "PerMonitorV2"), ("longPathAware", "true")] {
        assert_eq!(
            document
                .descendants()
                .find(|node| node
                    .has_tag_name(("http://schemas.microsoft.com/SMI/2016/WindowsSettings", tag)))
                .and_then(|node| node.text()),
            Some(value)
        );
    }
    Ok(())
}

#[test]
fn windows_static_crt_and_icon_resource_are_explicit() -> Result<()> {
    let config: toml::Table = fs::read_to_string(root().join(".cargo/config.toml"))?.parse()?;
    let flags = config["target"]["x86_64-pc-windows-msvc"]["rustflags"]
        .as_array()
        .context("missing flags")?;
    assert!(
        flags
            .iter()
            .any(|flag| flag.as_str() == Some("target-feature=+crt-static"))
    );
    let resource = fs::read_to_string(root().join("crates/app/resources/windows/app.rc"))?;
    assert!(resource.contains("1 ICON \"app-icon.ico\""));
    assert!(resource.contains("1 24 \"tgbooks.manifest\""));
    Ok(())
}

#[test]
fn bundle_validation_rejects_wrong_identity_version_or_missing_resources() -> Result<()> {
    let root = tempfile::tempdir()?;
    let bundle = root.path().join("Telegram Books.app");
    let contents = bundle.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources/licenses"))?;
    fs::write(contents.join("MacOS/tgbooks"), b"test")?;
    fs::write(contents.join("Resources/app-icon.icns"), b"test")?;
    for name in [
        "LICENSE",
        "NOTICE",
        "LICENSE-APACHE",
        "THIRD_PARTY_NOTICES.txt",
    ] {
        fs::write(contents.join("Resources/licenses").join(name), b"test")?;
    }
    let mut info = plist::Dictionary::new();
    for (key, value) in [
        ("CFBundleIdentifier", APP_ID),
        ("CFBundleName", APP_NAME),
        ("CFBundleExecutable", "tgbooks"),
        ("CFBundleShortVersionString", "0.1.0"),
        ("CFBundleIconFile", "app-icon.icns"),
        ("LSMinimumSystemVersion", "12.0"),
    ] {
        info.insert(key.into(), value.into());
    }
    info.insert("TgbooksOfferInstallation".into(), true.into());
    let path = contents.join("Info.plist");
    plist::Value::Dictionary(info.clone()).to_file_xml(&path)?;
    verify_bundle(&bundle, "0.1.0")?;
    for (key, value) in [
        ("CFBundleIdentifier", "other.app"),
        ("CFBundleShortVersionString", "0.0.0"),
        ("CFBundleIconFile", "../outside.icns"),
        ("CFBundleIconFile", ""),
    ] {
        let mut invalid = info.clone();
        invalid.insert(key.into(), value.into());
        plist::Value::Dictionary(invalid).to_file_xml(&path)?;
        assert!(verify_bundle(&bundle, "0.1.0").is_err());
    }
    let mut invalid = info.clone();
    invalid.insert("TgbooksOfferInstallation".into(), false.into());
    plist::Value::Dictionary(invalid).to_file_xml(&path)?;
    assert!(verify_bundle(&bundle, "0.1.0").is_err());
    plist::Value::Dictionary(info).to_file_xml(&path)?;
    fs::remove_file(contents.join("Resources/licenses/NOTICE"))?;
    assert!(verify_bundle(&bundle, "0.1.0").is_err());
    Ok(())
}

#[test]
fn missing_or_different_packager_version_fails_before_building() -> Result<()> {
    let root = tempfile::tempdir()?;
    assert!(check_tool(root.path()).is_err());
    assert!(validate_tool_version("cargo-packager 0.0.0").is_err());
    validate_tool_version(&format!("cargo-packager {}\n", PACKAGER_VERSION.trim()))?;
    assert!(tool_path(root.path()).starts_with(root.path().join("target/tools")));
    Ok(())
}

#[test]
fn nsis_selection_rejects_missing_or_ambiguous_installers() -> Result<()> {
    let root = tempfile::tempdir()?;
    assert!(find_installer(root.path()).is_err());
    fs::write(root.path().join("setup.exe"), b"test")?;
    fs::create_dir(root.path().join("not-a-file.exe"))?;
    assert_eq!(find_installer(root.path())?, root.path().join("setup.exe"));
    fs::write(root.path().join("other.exe"), b"test")?;
    assert!(find_installer(root.path()).is_err());
    Ok(())
}

#[test]
fn artifact_publication_replaces_only_the_archive_and_writes_sha256() -> Result<()> {
    let root = tempfile::tempdir()?;
    let staged = root.path().join("staged.zip");
    let final_path = root.path().join("Telegram Books.zip");
    fs::write(&staged, b"abc")?;
    fs::write(&final_path, b"old")?;
    publish_artifact(&staged, &final_path)?;
    assert!(!staged.exists());
    assert_eq!(fs::read(&final_path)?, b"abc");
    assert_eq!(
        fs::read_to_string(root.path().join("Telegram Books.zip.sha256"))?,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  Telegram Books.zip\n"
    );
    Ok(())
}
