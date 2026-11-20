//! values no spec can list because they live in the user's project or system: git refs and
//! make targets are read from the files that hold them, nothing runs. docker and podman
//! objects come from the program's own listing (`docker image ls --format ...`), bounded like
//! a help probe and kept for a few seconds

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use compcore_spec::runner::{self, Limits};

use super::sources;

/// a value and what it is
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub name: String,
    pub description: Option<String>,
}

impl Value {
    fn new(name: impl Into<String>, description: Option<&str>) -> Value {
        Value {
            name: name.into(),
            description: description.map(str::to_string),
        }
    }
}

/// values of a `git:`, `make:`, `docker:` or `podman:` template, none for other templates
pub fn values(template: &str, cwd: &Path, path_var: &str) -> Option<Vec<Value>> {
    let (source, kind) = template.split_once(':')?;
    match source {
        "git" => Some(git(kind, cwd)),
        "make" if kind == "targets" => Some(make_targets(cwd)),
        "docker" | "podman" => Some(containers(source, kind, path_var).to_vec()),
        _ => None,
    }
}

/// the git folder the repository's refs are in, the common one of a worktree
fn git_dir(cwd: &Path) -> Option<PathBuf> {
    let (top, dot) = cwd
        .ancestors()
        .map(|dir| (dir, dir.join(".git")))
        .find(|(_, dot)| dot.exists())?;
    let git_dir = match dot.is_dir() {
        true => dot,
        // a worktree or submodule has a `gitdir: <path>` file instead
        false => {
            let text = std::fs::read_to_string(&dot).ok()?;
            top.join(text.strip_prefix("gitdir:")?.trim())
        }
    };
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => Some(git_dir.join(text.trim())),
        Err(_) => Some(git_dir),
    }
}

fn git(kind: &str, cwd: &Path) -> Vec<Value> {
    let Some(common) = git_dir(cwd) else {
        return Vec::new();
    };
    let refs = |prefix: &str, description: &str| -> Vec<Value> {
        ref_names(&common, prefix)
            .into_iter()
            .filter(|name| name != "HEAD" && !name.ends_with("/HEAD"))
            .map(|name| Value::new(name, Some(description)))
            .collect()
    };
    match kind {
        "branches" => refs("refs/heads/", "branch"),
        "tags" => refs("refs/tags/", "tag"),
        "remotes" => remotes(&common),
        // the stash is shared by all worktrees, so its log is in the common folder
        "stashes" => stashes(&common),
        "refs" => [
            refs("refs/heads/", "branch"),
            refs("refs/remotes/", "remote branch"),
            refs("refs/tags/", "tag"),
        ]
        .concat(),
        _ => Vec::new(),
    }
}

/// names of the refs under `prefix`, from loose ref files and packed-refs, sorted
fn ref_names(common: &Path, prefix: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut dirs = vec![common.join(prefix)];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(name) = path.strip_prefix(common.join(prefix)) {
                names.push(name.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    if let Ok(packed) = std::fs::read_to_string(common.join("packed-refs")) {
        for line in packed.lines() {
            if let Some(name) = line
                .split_once(' ')
                .and_then(|(_, name)| name.strip_prefix(prefix))
            {
                names.push(name.to_string());
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

/// remotes from `[remote "name"]` sections of the config
fn remotes(common: &Path) -> Vec<Value> {
    let config = std::fs::read_to_string(common.join("config")).unwrap_or_default();
    let mut names: Vec<String> = config
        .lines()
        .filter_map(|line| line.trim().strip_prefix("[remote \"")?.strip_suffix("\"]"))
        .map(str::to_string)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .map(|name| Value::new(name, Some("remote")))
        .collect()
}

/// `stash@{n}` with its message, newest first. the stash's reflog lists the oldest first
fn stashes(common: &Path) -> Vec<Value> {
    let log = std::fs::read_to_string(common.join("logs/refs/stash")).unwrap_or_default();
    log.lines()
        .rev()
        .enumerate()
        .map(|(ix, line)| {
            let message = line.split_once('\t').map(|(_, message)| message);
            Value::new(format!("stash@{{{ix}}}"), message)
        })
        .collect()
}

/// targets of the makefile in `cwd`, as make picks it. a `## text` after a rule describes it
fn make_targets(cwd: &Path) -> Vec<Value> {
    let Some(text) = ["GNUmakefile", "makefile", "Makefile"]
        .iter()
        .find_map(|name| std::fs::read_to_string(cwd.join(name)).ok())
    else {
        return Vec::new();
    };
    let mut targets: Vec<Value> = Vec::new();
    for line in text.lines() {
        // recipes are indented by a tab
        if line.starts_with('\t') {
            continue;
        }
        let (rule, comment) = line.split_once('#').unwrap_or((line, ""));
        let Some((names, rest)) = rule.split_once(':') else {
            continue;
        };
        // `x := 1`, `x ::= 1` and `x = a:b` are variables
        if rest.starts_with('=') || rest.starts_with(":=") || names.contains('=') {
            continue;
        }
        let description = comment
            .strip_prefix('#')
            .map(str::trim)
            .filter(|text| !text.is_empty());
        for name in names.split_whitespace() {
            // `.PHONY` and friends are special targets, `%.o` a pattern
            let special = name.starts_with('.') || name.contains(['%', '$', '(', ')']);
            if !special && !targets.iter().any(|target| target.name == name) {
                targets.push(Value::new(name, description));
            }
        }
    }
    targets
}

// a few seconds keep the listing from running on every keystroke while still showing a new
// container soon
const CONTAINERS_TTL: Duration = Duration::from_secs(5);
const CONTAINERS_LIMITS: Limits = Limits {
    timeout: Duration::from_secs(1),
    max_bytes: 1024 * 1024,
};

type Listing = (Instant, Arc<Vec<Value>>);

static LISTINGS: Mutex<Option<HashMap<(String, String), Listing>>> = Mutex::new(None);

/// docker or podman objects of a kind, from the program's own listing
fn containers(program: &str, kind: &str, path_var: &str) -> Arc<Vec<Value>> {
    let key = (program.to_string(), kind.to_string());
    let mut cache = LISTINGS.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((at, values)) = cache.get(&key)
        && at.elapsed() < CONTAINERS_TTL
    {
        return values.clone();
    }
    let values = Arc::new(list_containers(program, kind, path_var));
    cache.insert(key, (Instant::now(), values.clone()));
    values
}

fn list_containers(program: &str, kind: &str, path_var: &str) -> Vec<Value> {
    let (args, description): (&[&str], &str) = match kind {
        "images" => (
            &["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"],
            "image",
        ),
        "containers" => (&["ps", "--all", "--format", "{{.Names}}"], "container"),
        "volumes" => (&["volume", "ls", "--format", "{{.Name}}"], "volume"),
        "networks" => (&["network", "ls", "--format", "{{.Name}}"], "network"),
        "pods" if program == "podman" => (&["pod", "ps", "--format", "{{.Name}}"], "pod"),
        _ => return Vec::new(),
    };
    let Some(path) = sources::program(program, path_var) else {
        return Vec::new();
    };
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let output = runner::run_stdout(&path, &args, None, CONTAINERS_LIMITS).unwrap_or_default();
    parse_listing(&output, description)
}

/// one name per line, without untagged images and repeats
fn parse_listing(output: &str, description: &str) -> Vec<Value> {
    let mut names: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.contains("<none>"))
        .filter(|name| !name.chars().any(char::is_control))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .map(|name| Value::new(name, Some(description)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("compcore-dynamic-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(values: &[Value]) -> Vec<&str> {
        values.iter().map(|value| value.name.as_str()).collect()
    }

    #[test]
    fn git_refs_come_from_loose_and_packed_refs() {
        let dir = temp("git");
        let git = dir.join(".git");
        std::fs::create_dir_all(git.join("refs/heads/feature")).unwrap();
        std::fs::create_dir_all(git.join("refs/remotes/origin")).unwrap();
        std::fs::create_dir_all(git.join("logs/refs")).unwrap();
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(git.join("refs/heads/main"), "x\n").unwrap();
        std::fs::write(git.join("refs/heads/feature/a"), "x\n").unwrap();
        std::fs::write(git.join("refs/remotes/origin/HEAD"), "ref: x\n").unwrap();
        std::fs::write(git.join("refs/remotes/origin/main"), "x\n").unwrap();
        std::fs::write(
            git.join("packed-refs"),
            "# pack-refs with: peeled\nabc refs/heads/old\nabc refs/tags/v1\n^def\n",
        )
        .unwrap();
        std::fs::write(
            git.join("config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = x\n[remote \"up\"]\n",
        )
        .unwrap();
        std::fs::write(
            git.join("logs/refs/stash"),
            "a b n <e> 1 +0000\tWIP on main: first\na b n <e> 2 +0000\tOn main: second\n",
        )
        .unwrap();
        // found from a folder inside the repository
        let cwd = dir.join("src/deep");
        assert_eq!(
            names(&git_values("branches", &cwd)),
            ["feature/a", "main", "old"]
        );
        assert_eq!(names(&git_values("tags", &cwd)), ["v1"]);
        assert_eq!(names(&git_values("remotes", &cwd)), ["origin", "up"]);
        assert_eq!(
            names(&git_values("refs", &cwd)),
            ["feature/a", "main", "old", "origin/main", "v1"]
        );
        let stashes = git_values("stashes", &cwd);
        assert_eq!(names(&stashes), ["stash@{0}", "stash@{1}"]);
        assert_eq!(stashes[0].description.as_deref(), Some("On main: second"));
        // a worktree points at its git folder, whose refs are in the common one
        let worktree = dir.join("wt");
        std::fs::create_dir_all(git.join("worktrees/wt")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(git.join("worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();
        assert_eq!(
            names(&git_values("branches", &worktree)),
            ["feature/a", "main", "old"]
        );
        assert_eq!(
            names(&git_values("stashes", &worktree)),
            ["stash@{0}", "stash@{1}"]
        );
        assert!(git_values("branches", Path::new("/")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn git_values(kind: &str, cwd: &Path) -> Vec<Value> {
        values(&format!("git:{kind}"), cwd, "").unwrap()
    }

    #[test]
    fn make_targets_skip_variables_patterns_and_special_targets() {
        let dir = temp("make");
        std::fs::write(
            dir.join("Makefile"),
            "CC := gcc\nX = a:b\n.PHONY: all test\nall: build ## build everything\n\
             build test:\n\t@echo a: b\n%.o: %.c\n$(OUT): x\ninstall:: all\n",
        )
        .unwrap();
        let targets = values("make:targets", &dir, "").unwrap();
        assert_eq!(names(&targets), ["all", "build", "test", "install"]);
        assert_eq!(targets[0].description.as_deref(), Some("build everything"));
        assert!(
            values("make:targets", &dir.join("none"), "")
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn listings_drop_untagged_images() {
        let parsed = parse_listing("b:latest\n<none>:<none>\na:1\n\nb:latest\n", "image");
        assert_eq!(names(&parsed), ["a:1", "b:latest"]);
        assert_eq!(values("other:x", Path::new("/"), ""), None);
    }

    #[cfg(unix)]
    #[test]
    fn listings_run_the_program_and_fail_soft() {
        let dir = temp("docker");
        crate::tests::write_program(
            &dir.join("podman"),
            "#!/bin/sh\n[ \"$1 $2\" = 'volume ls' ] && echo data\n",
        );
        let path_var = dir.to_string_lossy().into_owned();
        assert_eq!(
            names(&list_containers("podman", "volumes", &path_var)),
            ["data"]
        );
        // a failing listing and an unknown program give nothing
        assert!(list_containers("podman", "images", &path_var).is_empty());
        assert!(list_containers("docker", "volumes", &path_var).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
