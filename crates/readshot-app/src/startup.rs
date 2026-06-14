//! Launch-at-login integration.

use std::path::{Path, PathBuf};

use thiserror::Error;

const LAUNCH_AGENT_LABEL: &str = "np.com.pawanpaudel.readshot.login";
const LAUNCH_AGENT_FILENAME: &str = "np.com.pawanpaudel.readshot.login.plist";

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("home directory is unavailable")]
    HomeDirUnavailable,
    #[error("could not determine current executable: {0}")]
    CurrentExe(#[source] std::io::Error),
    #[error("current executable is not inside a Readshot.app bundle: {0}")]
    UnsupportedBundleLayout(PathBuf),
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Enable or disable launching Readshot when the user logs in.
///
/// macOS loads user LaunchAgents from `~/Library/LaunchAgents` during
/// login. We store an agent that launches the `.app` bundle through
/// Launch Services, preserving the app identity used for TCC and URL
/// scheme registration.
#[cfg(target_os = "macos")]
pub fn set_launch_at_login(enabled: bool) -> Result<(), StartupError> {
    let plist_path = launch_agent_path()?;

    if enabled {
        let exe = std::env::current_exe().map_err(StartupError::CurrentExe)?;
        let bundle = bundle_path_from_exe(&exe)
            .ok_or_else(|| StartupError::UnsupportedBundleLayout(exe.clone()))?;
        write_launch_agent(&plist_path, &bundle)
    } else {
        remove_launch_agent(&plist_path)
    }
}

/// Non-macOS builds keep the preference schema portable, but the
/// startup integration itself is macOS-only for now.
#[cfg(not(target_os = "macos"))]
pub fn set_launch_at_login(_enabled: bool) -> Result<(), StartupError> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Result<PathBuf, StartupError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(StartupError::HomeDirUnavailable)?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(LAUNCH_AGENT_FILENAME))
}

#[cfg(target_os = "macos")]
fn write_launch_agent(plist_path: &Path, bundle_path: &Path) -> Result<(), StartupError> {
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StartupError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    std::fs::write(plist_path, launch_agent_plist(bundle_path)).map_err(|source| StartupError::Io {
        path: plist_path.to_path_buf(),
        source,
    })
}

#[cfg(target_os = "macos")]
fn remove_launch_agent(plist_path: &Path) -> Result<(), StartupError> {
    match std::fs::remove_file(plist_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StartupError::Io {
            path: plist_path.to_path_buf(),
            source,
        }),
    }
}

fn launch_agent_plist(app_bundle_path: &Path) -> String {
    let bundle = escape_plist_string(&app_bundle_path.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/open</string>
    <string>{bundle}</string>
    <string>--args</string>
    <string>--launched-at-login</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

fn bundle_path_from_exe(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()? != "Contents" {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    if app_dir.extension()? != "app" {
        return None;
    }
    Some(app_dir.to_path_buf())
}

fn escape_plist_string(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn plist_opens_the_app_bundle_at_login() {
        let plist = super::launch_agent_plist(Path::new("/Applications/Readshot.app"));

        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>np.com.pawanpaudel.readshot.login</string>"));
        assert!(plist.contains("<string>/usr/bin/open</string>"));
        assert!(plist.contains("<string>/Applications/Readshot.app</string>"));
        // The login-launch marker lets the app skip the ready window and
        // settle silently into the menu bar instead of popping a window
        // on every boot.
        assert!(plist.contains("<string>--args</string>"));
        assert!(plist.contains("<string>--launched-at-login</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<true/>"));
    }

    #[test]
    fn bundle_path_is_detected_from_app_executable() {
        let exe = Path::new("/Applications/Readshot.app/Contents/MacOS/readshot");

        assert_eq!(
            super::bundle_path_from_exe(exe).as_deref(),
            Some(Path::new("/Applications/Readshot.app")),
        );
    }

    #[test]
    fn bundle_path_is_not_detected_from_plain_binary() {
        let exe = Path::new("/usr/local/bin/readshot");

        assert!(super::bundle_path_from_exe(exe).is_none());
    }

    #[test]
    fn plist_escapes_bundle_path() {
        let plist = super::launch_agent_plist(Path::new("/Applications/Read & Shot.app"));

        assert!(plist.contains("<string>/Applications/Read &amp; Shot.app</string>"));
    }
}
