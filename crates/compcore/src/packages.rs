//! packages package managers already know about, from the caches they keep on disk. the
//! managers are only asked to list their cache: cache-only options keep the queries off the
//! network, and nothing else is run

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use super::sources;

/// package managers whose packages can be completed
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Manager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Apk,
    Brew,
    Flatpak,
}

// dnf's repoquery, the same for dnf5 and dnf4
const DNF_QUERY: &[&str] = &[
    "-C",
    "repoquery",
    "--available",
    "--queryformat",
    "%{name}\\n",
];

impl Manager {
    /// manager of a `packages:<manager>` template, none for unknown ones
    pub fn from_template(name: &str) -> Option<Manager> {
        match name {
            "apt" => Some(Manager::Apt),
            "dnf" | "yum" => Some(Manager::Dnf),
            "pacman" => Some(Manager::Pacman),
            "zypper" => Some(Manager::Zypper),
            "apk" => Some(Manager::Apk),
            "brew" => Some(Manager::Brew),
            "flatpak" => Some(Manager::Flatpak),
            _ => None,
        }
    }

    /// programs and the argument lists that list the names in the manager's local cache. the
    /// first program found on PATH answers, with the output of all its lists. they only read
    /// what the manager downloaded before
    fn queries(self) -> &'static [(&'static str, &'static [&'static [&'static str]])] {
        match self {
            Manager::Apt => &[("apt-cache", &[&["pkgnames"]])],
            Manager::Dnf => &[("dnf5", &[DNF_QUERY]), ("dnf", &[DNF_QUERY])],
            Manager::Pacman => &[("pacman", &[&["-Ssq"]])],
            // --no-refresh keeps zypper from fetching expired repositories
            Manager::Zypper => &[(
                "zypper",
                &[&["--no-refresh", "-q", "search", "-t", "package"]],
            )],
            Manager::Apk => &[("apk", &[&["search", "-q"]])],
            Manager::Brew => &[("brew", &[&["formulae"], &["casks"]])],
            Manager::Flatpak => &[(
                "flatpak",
                &[&["remote-ls", "--cached", "--app", "--columns=application"]],
            )],
        }
    }
}

type Names = HashMap<Manager, Arc<Vec<String>>>;

static NAMES: Mutex<Option<Names>> = Mutex::new(None);

/// names in the manager's local cache, sorted. read once per process: the commands take long
/// enough to not run them per keystroke, and one run by the user meanwhile shows after restart
pub fn names(manager: Manager, path_var: &str) -> Arc<Vec<String>> {
    let mut cache = NAMES.lock().unwrap();
    cache
        .get_or_insert_with(HashMap::new)
        .entry(manager)
        .or_insert_with(|| Arc::new(read(manager, path_var)))
        .clone()
}

/// use `names` instead of querying a cache
#[cfg(test)]
pub fn remember(manager: Manager, names: Vec<String>) {
    NAMES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(manager, Arc::new(names));
}

fn read(manager: Manager, path_var: &str) -> Vec<String> {
    for &(program, lists) in manager.queries() {
        let Some(path) = sources::program(program, path_var) else {
            continue;
        };
        let mut text = String::new();
        for args in lists {
            let Ok(output) = std::process::Command::new(&path).args(*args).output() else {
                continue;
            };
            if output.status.success() {
                text.push_str(&String::from_utf8_lossy(&output.stdout));
            }
        }
        if !text.is_empty() {
            return parse(&text);
        }
    }
    Vec::new()
}

/// package names in the output of a query, one per line, without repeats. a table row like
/// zypper's `i | name | summary | type` gives its second column
fn parse(output: &str) -> Vec<String> {
    let mut names: Vec<String> = output
        .lines()
        .map(|line| match line.contains('|') {
            true => line.split('|').nth(1).unwrap_or_default().trim(),
            false => line.trim(),
        })
        .filter(|name| !name.is_empty() && *name != "Name")
        .filter(|name| !name.chars().all(|c| c == '-' || c == '+'))
        .filter(|name| !name.chars().any(char::is_control))
        .map(str::to_string)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_map_to_their_manager() {
        assert_eq!(Manager::from_template("apt"), Some(Manager::Apt));
        assert_eq!(Manager::from_template("yum"), Some(Manager::Dnf));
        assert_eq!(Manager::from_template("brew"), Some(Manager::Brew));
        assert_eq!(Manager::from_template("git"), None);
    }

    #[test]
    fn names_are_cleaned_and_sorted() {
        let parsed = parse("vim\nfirefox\r\n\nfirefox\n  \nfirefox-esr\n");
        assert_eq!(parsed, ["firefox", "firefox-esr", "vim"]);
        // a name with a control character in it can't be typed back at a prompt
        assert_eq!(parse("a\nb\tc\n"), ["a"]);
        // zypper's table, header and rule dropped
        let table = "S | Name | Summary | Type\n--+------+---------+--------\n  | vim  | Vi IMproved | package\n";
        assert_eq!(parse(table), ["vim"]);
    }

    #[cfg(unix)]
    #[test]
    fn queries_use_the_reported_path_and_fail_soft() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("compcore-packages-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("apt-cache");
        std::fs::write(&cache, "#!/bin/sh\necho firefox\necho firefox-esr\n").unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path_var = dir.to_string_lossy().into_owned();
        assert_eq!(read(Manager::Apt, &path_var), ["firefox", "firefox-esr"]);
        // no program, and a program that fails, give nothing
        assert!(read(Manager::Pacman, &path_var).is_empty());
        std::fs::write(&cache, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(read(Manager::Apt, &path_var).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
