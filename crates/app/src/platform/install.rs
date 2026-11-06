//! macOS-only installation. No Telegram, session, SQLite or elevated privileges.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use rustix::fs::{CWD, RenameFlags, renameat_with};

use super::{APP_ID, APP_NAME};

const INSTALL_KEY: &str = "TgbooksOfferInstallation";

#[derive(Clone, Debug)]
pub(crate) struct Locations {
    pub system: PathBuf,
    pub user: Option<PathBuf>,
}

impl Locations {
    pub fn system() -> Self {
        Self {
            system: "/Applications".into(),
            user: std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join("Applications")),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Proposal {
    pub source: PathBuf,
    pub locations: Locations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Destination {
    System,
    User,
}

impl Proposal {
    pub fn directory(&self, destination: Destination) -> Option<&Path> {
        match destination {
            Destination::System => Some(&self.locations.system),
            Destination::User => self.locations.user.as_deref(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallError {
    InvalidBundle,
    Occupied,
    Permission,
    Copy,
    Signature,
    Publication,
}

impl InstallError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Occupied => {
                "В выбранной папке уже есть Telegram Books.app. Существующая копия не изменена. Закройте её и при необходимости замените вручную в Finder."
            }
            Self::Permission => {
                "Не удалось записать приложение в выбранную папку. Можно продолжить запуск здесь или установить приложение вручную в ~/Applications. Права администратора автоматически не запрашиваются."
            }
            Self::InvalidBundle => {
                "Не удалось проверить структуру приложения. Продолжите запуск здесь или установите заново из проверенного архива."
            }
            Self::Copy | Self::Publication => {
                "Не удалось завершить копирование приложения. Исходная копия сохранена; можно продолжить запуск здесь."
            }
            Self::Signature => {
                "Подпись скопированного приложения не прошла проверку. Эта копия не установлена; исходное приложение сохранено."
            }
        }
    }
}

fn bundle_info(bundle: &Path) -> Result<plist::Dictionary, InstallError> {
    if bundle.extension().is_none_or(|ext| ext != "app") {
        return Err(InstallError::InvalidBundle);
    }
    let info = bundle.join("Contents/Info.plist");
    let size = fs::metadata(&info)
        .map_err(|_| InstallError::InvalidBundle)?
        .len();
    if size > 1024 * 1024 {
        return Err(InstallError::InvalidBundle);
    }
    let info = plist::Value::from_file(info)
        .map_err(|_| InstallError::InvalidBundle)?
        .into_dictionary()
        .ok_or(InstallError::InvalidBundle)?;
    if info
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        != Some(APP_ID)
        || info
            .get("CFBundleExecutable")
            .and_then(plist::Value::as_string)
            != Some("tgbooks")
        || info.get(INSTALL_KEY).and_then(plist::Value::as_boolean) != Some(true)
    {
        return Err(InstallError::InvalidBundle);
    }
    let executable = bundle
        .join("Contents/MacOS/tgbooks")
        .canonicalize()
        .map_err(|_| InstallError::InvalidBundle)?;
    let canonical = bundle
        .canonicalize()
        .map_err(|_| InstallError::InvalidBundle)?;
    if !executable.starts_with(canonical) || !executable.is_file() {
        return Err(InstallError::InvalidBundle);
    }
    Ok(info)
}

/// Resolve symlinks, including an alias outside Applications pointing inside it.
/// A directory's path components, not a string prefix, define "installed".
pub(crate) fn proposal(bundle: &Path, locations: Locations, release: bool) -> Option<Proposal> {
    if !release || bundle_info(bundle).is_err() {
        return None;
    }
    let source = bundle.canonicalize().ok()?;
    let installed = std::iter::once(locations.system.as_path())
        .chain(locations.user.as_deref())
        .any(|root| source.starts_with(root.canonicalize().unwrap_or_else(|_| root.to_owned())));
    if installed {
        None
    } else {
        Some(Proposal { source, locations })
    }
}

fn absent(path: &Path) -> Result<(), InstallError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(InstallError::Occupied),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(InstallError::Permission),
    }
}

fn publish(source: &Path, destination: &Path) -> Result<(), InstallError> {
    // RENAME_EXCL on macOS: no overwrite even if a destination appears after
    // the initial check. std::fs::rename alone would replace an empty directory.
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            InstallError::Occupied
        } else {
            InstallError::Publication
        }
    })
}

pub(crate) fn install(plan: Proposal, destination: Destination) -> Result<PathBuf, InstallError> {
    bundle_info(&plan.source)?;
    let directory = plan
        .directory(destination)
        .ok_or(InstallError::Permission)?;
    fs::create_dir_all(directory).map_err(|_| InstallError::Permission)?;
    let directory = directory
        .canonicalize()
        .map_err(|_| InstallError::Permission)?;
    let installed = directory.join(format!("{APP_NAME}.app"));
    absent(&installed)?;
    // Only this private staging folder is removed on failure. Never remove the
    // source bundle, an existing app, quarantine attributes or user data.
    let staging = tempfile::Builder::new()
        .prefix(".tgbooks-install-")
        .tempdir_in(&directory)
        .map_err(|_| InstallError::Permission)?;
    let copied = staging.path().join(format!("{APP_NAME}.app"));
    let copy = Command::new("/usr/bin/ditto")
        .args(["--rsrc", "--extattr", "--qtn"])
        .env_remove("DITTONORSRC")
        .arg(&plan.source)
        .arg(&copied)
        .output()
        .map_err(|_| InstallError::Copy)?;
    if !copy.status.success() {
        return Err(InstallError::Copy);
    }
    bundle_info(&copied)?;
    let verify = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&copied)
        .output()
        .map_err(|_| InstallError::Signature)?;
    if !verify.status.success() {
        return Err(InstallError::Signature);
    }
    publish(&copied, &installed)?;
    Ok(installed)
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
