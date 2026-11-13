//! What `clayo [folder] [--background]` asks for (App.xaml.cs OnStartup / Chosen).

use crate::paths::{normalize, same_dir};
use crate::system::login_startup::BACKGROUND_ARG;
use std::path::Path;

/// The parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartArgs {
    /// Started at login: no window, only the island, until asked for.
    pub background: bool,
    /// Where to start a session; None opens on the home folder with no session.
    pub folder: Option<String>,
}

/// The first argument that is an existing folder (any position) wins. Else, unless started in
/// the background, the working directory, unless nobody chose it: a shortcut or the Start menu
/// starts Clayo in its own folder, and some launchers in System32.
pub fn parse(
    args: &[String],
    cwd: &Path,
    exe_dir: &Path,
    system_dir: &Path,
    is_dir: impl Fn(&str) -> bool,
) -> StartArgs {
    let background = args.iter().any(|a| a == BACKGROUND_ARG);
    let folder = args.iter().find(|a| is_dir(a)).cloned().or_else(|| {
        if background {
            return None;
        }
        let dir = normalize(cwd);
        let unchosen = same_dir(&dir, &normalize(exe_dir)) || same_dir(&dir, &normalize(system_dir));
        (!unchosen).then(|| cwd.to_string_lossy().into_owned())
    });
    StartArgs { background, folder }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], cwd: &str) -> StartArgs {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(
            &args,
            Path::new(cwd),
            Path::new(r"C:\Users\me\AppData\Local\Programs\Clayo\"),
            Path::new(r"C:\Windows\system32"),
            |p| p.starts_with(r"D:\"),
        )
    }

    #[test]
    fn first_existing_folder_argument_wins_anywhere() {
        let a = run(&["--x", r"D:\work", r"D:\other"], r"C:\cwd");
        assert_eq!(a.folder.as_deref(), Some(r"D:\work"));
        assert!(!a.background);
    }

    #[test]
    fn cwd_unless_exe_folder_or_system32() {
        assert_eq!(run(&[], r"C:\proj").folder.as_deref(), Some(r"C:\proj"));
        assert_eq!(run(&[], r"c:\users\ME\AppData\Local\Programs\Clayo").folder, None);
        assert_eq!(run(&[], r"C:\Windows\System32\").folder, None);
        assert_eq!(run(&["nope"], r"C:\Windows\System32").folder, None);
    }

    #[test]
    fn background_ignores_cwd_but_not_a_folder_argument() {
        let a = run(&["--background"], r"C:\proj");
        assert!(a.background);
        assert_eq!(a.folder, None);
        assert_eq!(run(&["--background", r"D:\w"], r"C:\proj").folder.as_deref(), Some(r"D:\w"));
    }
}
