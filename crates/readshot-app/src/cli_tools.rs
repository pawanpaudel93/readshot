use std::fs;
use std::path::{Path, PathBuf};

const COMMANDS: &[&str] = &["readshot", "readshot-mcp"];
const USER_BIN_DIR_NAME: &str = "bin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub bin_dir: PathBuf,
    pub created: usize,
    pub already_installed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CliToolsError {
    #[error("Readshot is not running from a .app bundle")]
    NotInAppBundle,
    #[error("missing packaged command `{command}` at {path}")]
    MissingExecutable { command: String, path: PathBuf },
    #[error("refusing to overwrite existing `{command}` at {path}")]
    ExistingPath { command: String, path: PathBuf },
    #[error("{operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn install_command_line_tools() -> Result<InstallReport, CliToolsError> {
    let app_bundle = current_app_bundle()?;
    let bin_dir = default_user_bin_dir()?;
    install_command_line_tools_into(&app_bundle, &bin_dir)
}

pub fn install_command_line_tools_into(
    app_bundle: &Path,
    bin_dir: &Path,
) -> Result<InstallReport, CliToolsError> {
    fs::create_dir_all(bin_dir).map_err(|source| CliToolsError::Io {
        operation: "create bin directory",
        path: bin_dir.to_path_buf(),
        source,
    })?;

    let macos_dir = app_bundle.join("Contents/MacOS");
    let mut report = InstallReport {
        bin_dir: bin_dir.to_path_buf(),
        created: 0,
        already_installed: 0,
    };

    for command in COMMANDS {
        let target = macos_dir.join(command);
        if !target.is_file() {
            return Err(CliToolsError::MissingExecutable {
                command: (*command).to_string(),
                path: target,
            });
        }

        let link = bin_dir.join(command);
        match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let existing = fs::read_link(&link).map_err(|source| CliToolsError::Io {
                    operation: "read symlink",
                    path: link.clone(),
                    source,
                })?;
                if existing == target {
                    report.already_installed += 1;
                } else {
                    return Err(CliToolsError::ExistingPath {
                        command: (*command).to_string(),
                        path: link,
                    });
                }
            }
            Ok(_) => {
                return Err(CliToolsError::ExistingPath {
                    command: (*command).to_string(),
                    path: link,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                symlink(&target, &link)?;
                report.created += 1;
            }
            Err(source) => {
                return Err(CliToolsError::Io {
                    operation: "inspect command path",
                    path: link,
                    source,
                });
            }
        }
    }

    Ok(report)
}

fn current_app_bundle() -> Result<PathBuf, CliToolsError> {
    let exe = std::env::current_exe().map_err(|source| CliToolsError::Io {
        operation: "locate current executable",
        path: PathBuf::from("readshot"),
        source,
    })?;
    let Some(bundle) = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
    else {
        return Err(CliToolsError::NotInAppBundle);
    };

    if bundle.extension().and_then(|e| e.to_str()) == Some("app") {
        Ok(bundle)
    } else {
        Err(CliToolsError::NotInAppBundle)
    }
}

fn default_user_bin_dir() -> Result<PathBuf, CliToolsError> {
    let home = std::env::var_os("HOME").ok_or_else(|| CliToolsError::Io {
        operation: "locate home directory",
        path: PathBuf::from("HOME"),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"),
    })?;
    Ok(PathBuf::from(home).join(USER_BIN_DIR_NAME))
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> Result<(), CliToolsError> {
    std::os::unix::fs::symlink(target, link).map_err(|source| CliToolsError::Io {
        operation: "create symlink",
        path: link.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn symlink(_target: &Path, link: &Path) -> Result<(), CliToolsError> {
    Err(CliToolsError::Io {
        operation: "create symlink",
        path: link.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "command-line tool symlinks are only supported on Unix",
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn installs_readshot_and_mcp_symlinks_into_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Readshot.app");
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("readshot"), b"readshot").unwrap();
        fs::write(macos.join("readshot-mcp"), b"readshot-mcp").unwrap();

        let bin = dir.path().join("bin");
        let report = install_command_line_tools_into(&app, &bin).unwrap();

        assert_eq!(report.created, 2);
        assert_eq!(
            fs::read_link(bin.join("readshot")).unwrap(),
            macos.join("readshot")
        );
        assert_eq!(
            fs::read_link(bin.join("readshot-mcp")).unwrap(),
            macos.join("readshot-mcp")
        );
    }

    #[test]
    fn existing_correct_symlinks_are_left_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Readshot.app");
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("readshot"), b"readshot").unwrap();
        fs::write(macos.join("readshot-mcp"), b"readshot-mcp").unwrap();

        let bin = dir.path().join("bin");
        install_command_line_tools_into(&app, &bin).unwrap();
        let report = install_command_line_tools_into(&app, &bin).unwrap();

        assert_eq!(report.created, 0);
        assert_eq!(report.already_installed, 2);
    }

    #[test]
    fn refuses_to_overwrite_existing_non_matching_command() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Readshot.app");
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("readshot"), b"readshot").unwrap();
        fs::write(macos.join("readshot-mcp"), b"readshot-mcp").unwrap();

        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("readshot"), b"other command").unwrap();

        let err = install_command_line_tools_into(&app, &bin).unwrap_err();

        assert!(
            matches!(err, CliToolsError::ExistingPath { command, .. } if command == "readshot")
        );
    }

    #[test]
    fn default_user_bin_dir_uses_home_bin() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());

        assert_eq!(default_user_bin_dir().unwrap(), dir.path().join("bin"));
    }
}
