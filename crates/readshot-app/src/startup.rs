//! Launch-at-login integration.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[cfg(any(target_os = "macos", test))]
const LAUNCH_AGENT_LABEL: &str = "np.com.pawanpaudel.readshot.login";
#[cfg(target_os = "macos")]
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
    /// A helper the OS provides (Windows `reg.exe`) failed.
    #[error("{0}")]
    Command(String),
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

/// Linux: an XDG autostart entry, `$XDG_CONFIG_HOME/autostart/
/// readshot.desktop` (`~/.config` when the variable is unset or not
/// absolute), which desktop sessions start at login. An AppImage
/// registers the AppImage file (`$APPIMAGE`), not its temporary mount.
#[cfg(target_os = "linux")]
pub fn set_launch_at_login(enabled: bool) -> Result<(), StartupError> {
    let path = xdg_autostart_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )?;
    if enabled {
        let exe = match std::env::var_os("APPIMAGE") {
            Some(appimage) if !appimage.is_empty() => PathBuf::from(appimage),
            _ => std::env::current_exe().map_err(StartupError::CurrentExe)?,
        };
        write_login_file(&path, &autostart_desktop_entry(&exe))
    } else {
        remove_login_file(&path)
    }
}

/// Windows: a `Readshot` value under the per-user Run key, written and
/// removed with `reg.exe` (no extra crate).
#[cfg(target_os = "windows")]
pub fn set_launch_at_login(enabled: bool) -> Result<(), StartupError> {
    if enabled {
        let exe = std::env::current_exe().map_err(StartupError::CurrentExe)?;
        run_reg(&windows_run_add_args(&exe)).map(|_| ())
    } else {
        // Nothing to delete is success: `reg delete` fails then, so
        // only a value that is still there afterwards is an error.
        match run_reg(&windows_run_delete_args()) {
            Ok(_) => Ok(()),
            Err(e) => match run_reg(&windows_run_query_args()) {
                Ok(_) => Err(e),
                Err(_) => Ok(()),
            },
        }
    }
}

#[cfg(target_os = "windows")]
fn run_reg(args: &[String]) -> Result<(), StartupError> {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up for `reg.exe`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|source| StartupError::Io {
            path: PathBuf::from("reg.exe"),
            source,
        })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(StartupError::Command(format!(
            "reg {} failed: {}",
            args.first().map(String::as_str).unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The per-user Run key and the value Readshot owns in it.
#[cfg(any(target_os = "windows", test))]
const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(any(target_os = "windows", test))]
const WINDOWS_RUN_VALUE: &str = "Readshot";

/// `reg add` arguments that start `exe` at login with the login marker.
/// The executable is quoted, so a path with spaces stays one argument.
#[cfg(any(target_os = "windows", test))]
fn windows_run_add_args(exe: &Path) -> Vec<String> {
    vec![
        "add".into(),
        WINDOWS_RUN_KEY.into(),
        "/v".into(),
        WINDOWS_RUN_VALUE.into(),
        "/t".into(),
        "REG_SZ".into(),
        "/d".into(),
        format!("\"{}\" --launched-at-login", exe.display()),
        "/f".into(),
    ]
}

#[cfg(any(target_os = "windows", test))]
fn windows_run_delete_args() -> Vec<String> {
    vec![
        "delete".into(),
        WINDOWS_RUN_KEY.into(),
        "/v".into(),
        WINDOWS_RUN_VALUE.into(),
        "/f".into(),
    ]
}

#[cfg(any(target_os = "windows", test))]
fn windows_run_query_args() -> Vec<String> {
    vec![
        "query".into(),
        WINDOWS_RUN_KEY.into(),
        "/v".into(),
        WINDOWS_RUN_VALUE.into(),
    ]
}

/// Where the XDG autostart entry lives, from `$XDG_CONFIG_HOME` and
/// `$HOME`. The spec ignores a relative `XDG_CONFIG_HOME`.
#[cfg(any(target_os = "linux", test))]
fn xdg_autostart_path(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, StartupError> {
    let config = match xdg_config_home.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => home
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".config"))
            .ok_or(StartupError::HomeDirUnavailable)?,
    };
    Ok(config.join("autostart").join(XDG_AUTOSTART_FILENAME))
}

#[cfg(any(target_os = "linux", test))]
const XDG_AUTOSTART_FILENAME: &str = "readshot.desktop";

/// The autostart `.desktop` entry that runs `exe` with the login marker.
#[cfg(any(target_os = "linux", test))]
fn autostart_desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Readshot\n\
         Comment=Screenshot and text recognition in the tray\n\
         Exec={} --launched-at-login\n\
         Icon=readshot\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        desktop_exec_quote(&exe.to_string_lossy())
    )
}

/// Quote one `Exec=` argument per the Desktop Entry spec: inside double
/// quotes, `"`, `` ` ``, `$` and `\` are backslash-escaped; then the
/// string-value escape doubles every backslash; `%` is a field code
/// unless written `%%`.
#[cfg(any(target_os = "linux", test))]
fn desktop_exec_quote(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(any(target_os = "linux", test))]
fn write_login_file(path: &Path, contents: &str) -> Result<(), StartupError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StartupError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, contents).map_err(|source| StartupError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(any(target_os = "linux", test))]
fn remove_login_file(path: &Path) -> Result<(), StartupError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StartupError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
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

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
fn escape_plist_string(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn autostart_entry_lives_under_xdg_config_home_or_home_config() {
        assert_eq!(
            super::xdg_autostart_path(Some(OsString::from("/cfg")), Some(OsString::from("/h")))
                .unwrap(),
            PathBuf::from("/cfg/autostart/readshot.desktop")
        );
        // Unset, empty or relative XDG_CONFIG_HOME: ~/.config.
        for xdg in [None, Some(OsString::new()), Some(OsString::from("rel"))] {
            assert_eq!(
                super::xdg_autostart_path(xdg, Some(OsString::from("/home/a"))).unwrap(),
                PathBuf::from("/home/a/.config/autostart/readshot.desktop")
            );
        }
        assert!(matches!(
            super::xdg_autostart_path(None, None),
            Err(super::StartupError::HomeDirUnavailable)
        ));
    }

    #[test]
    fn autostart_entry_runs_the_executable_with_the_login_marker() {
        let entry = super::autostart_desktop_entry(Path::new("/opt/Read Shot/readshot"));
        assert!(entry.starts_with("[Desktop Entry]\n"), "{entry}");
        assert!(entry.contains("\nType=Application\n"), "{entry}");
        assert!(
            entry.contains("\nExec=\"/opt/Read Shot/readshot\" --launched-at-login\n"),
            "{entry}"
        );
        assert!(
            entry.contains("\nX-GNOME-Autostart-enabled=true\n"),
            "{entry}"
        );
    }

    #[test]
    fn exec_quoting_escapes_what_the_desktop_entry_spec_reserves() {
        assert_eq!(super::desktop_exec_quote("/a b"), "\"/a b\"");
        assert_eq!(super::desktop_exec_quote("/a\"b"), "\"/a\\\\\"b\"");
        assert_eq!(super::desktop_exec_quote("/$x`y"), "\"/\\\\$x\\\\`y\"");
        assert_eq!(super::desktop_exec_quote("/100%"), "\"/100%%\"");
        // A literal backslash takes four in the file.
        assert_eq!(super::desktop_exec_quote(r"/a\b"), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn writing_then_removing_the_login_file_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("autostart").join("readshot.desktop");
        super::write_login_file(&path, "x").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        super::remove_login_file(&path).unwrap();
        assert!(!path.exists());
        // Removing what isn't there is fine (toggle off twice).
        super::remove_login_file(&path).unwrap();
    }

    #[test]
    fn windows_run_value_quotes_the_executable_and_marks_the_login_launch() {
        let add = super::windows_run_add_args(Path::new(r"C:\Program Files\Readshot\readshot.exe"));
        assert_eq!(
            add,
            [
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Readshot",
                "/t",
                "REG_SZ",
                "/d",
                r#""C:\Program Files\Readshot\readshot.exe" --launched-at-login"#,
                "/f",
            ]
        );
        assert_eq!(
            super::windows_run_delete_args(),
            [
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Readshot",
                "/f",
            ]
        );
        assert_eq!(super::windows_run_query_args()[0], "query");
    }

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
