//! help of commands no bundled spec knows, run once and cached. the command is one the user
//! is already typing and about to run, and only its own help is asked for: nothing it does
//! is run, and `autocomplete.probe_help` turns the whole thing off

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, UNIX_EPOCH},
};

use compcore_spec::{
    Spec, help, merge_child,
    runner::{self, Limits},
};
use serde::{Deserialize, Serialize};

// bump when the help parser changes, old cache files then stop being read
const VERSION: u32 = 1;
const TIMEOUT: Duration = Duration::from_millis(1000);
const MAX_BYTES: usize = 256 * 1024;
// a command with more subcommands than this gets help for the first few only
const MAX_SUBCOMMANDS: usize = 24;

// one probe's spec, filled by the first thread that asks for it
type Slot = Arc<OnceLock<Option<Arc<Spec>>>>;

/// parsed specs by `program --help` and its subcommands
pub struct Prober {
    cache_dir: Option<PathBuf>,
    limits: Limits,
    memory: Mutex<HashMap<String, Slot>>,
}

impl Default for Prober {
    fn default() -> Self {
        Prober::new(None)
    }
}

impl Prober {
    /// cache probes in `cache_dir`, none keeps them in memory only
    pub fn new(cache_dir: Option<PathBuf>) -> Prober {
        Prober {
            cache_dir,
            limits: Limits {
                timeout: TIMEOUT,
                max_bytes: MAX_BYTES,
            },
            memory: Mutex::new(HashMap::new()),
        }
    }

    /// the spec of `command`, read from `program`'s help the first time. none when the help
    /// says nothing and when the program can't be run
    pub fn spec(&self, command: &str, program: &Path, cwd: Option<&Path>) -> Option<Arc<Spec>> {
        let meta = Meta::read(program);
        let key = format!(
            "{command}:{:016x}",
            hash(&[command, &meta.path, &meta.len, &meta.mtime])
        );
        let slot = {
            let mut memory = self.memory.lock().unwrap();
            memory
                .entry(key)
                .or_insert_with(|| Arc::new(OnceLock::new()))
                .clone()
        };
        // one thread runs the help, the others wait for it instead of running it again
        slot.get_or_init(|| self.probe(command, program, cwd, &meta))
            .clone()
    }

    fn probe(
        &self,
        command: &str,
        program: &Path,
        cwd: Option<&Path>,
        meta: &Meta,
    ) -> Option<Arc<Spec>> {
        if let Some(cached) = self.read_cache(command, meta) {
            return cached;
        }
        let mut spec = self.run(command, program, &["--help".into()], cwd);
        if !spec.usable() {
            let short = self.run(command, program, &["-h".into()], cwd);
            if short.usable() {
                spec = short;
            }
        }
        let spec = match spec.usable() {
            true => Some(Arc::new(self.with_subcommands(spec, command, program, cwd))),
            false => None,
        };
        self.write_cache(command, meta, &spec);
        spec
    }

    fn run(&self, command: &str, program: &Path, args: &[String], cwd: Option<&Path>) -> Spec {
        self.run_as(command, command, program, args, cwd)
    }

    /// parse help whose command rows carry `prefix`, like yargs' `tool sub command`
    fn run_as(
        &self,
        name: &str,
        prefix: &str,
        program: &Path,
        args: &[String],
        cwd: Option<&Path>,
    ) -> Spec {
        let text = runner::run_help(program, args, cwd, self.limits);
        help::parse_help_as(name, &text, prefix, &help::Hints::default())
    }

    /// ask each top-level subcommand for its own help, so its options complete too. one
    /// level only: completing `cmd sub deeper --` would need a run per keystroke and is not
    /// worth it
    fn with_subcommands(
        &self,
        mut spec: Spec,
        command: &str,
        program: &Path,
        cwd: Option<&Path>,
    ) -> Spec {
        let subcommands = std::mem::take(&mut spec.subcommands);
        if !subcommands.is_empty() {
            // options listed by the command itself are usually global ones
            for option in &mut spec.options {
                option.is_persistent = true;
            }
        }
        for (ix, mut sub) in subcommands.into_iter().enumerate() {
            if ix < MAX_SUBCOMMANDS {
                let name = sub.name.all()[0].clone();
                let prefix = format!("{command} {name}");
                let parsed = self.run_as(
                    &name,
                    &prefix,
                    program,
                    &[name.clone(), "--help".into()],
                    cwd,
                );
                let parsed = match parsed.usable() {
                    true => parsed,
                    false => {
                        self.run_as(&name, &prefix, program, &["help".into(), name.clone()], cwd)
                    }
                };
                merge_child(&mut sub, parsed);
            }
            spec.subcommands.push(sub);
        }
        spec
    }

    fn cache_path(&self, command: &str, meta: &Meta) -> Option<PathBuf> {
        let dir = self.cache_dir.as_ref()?;
        let safe: String = command
            .chars()
            .map(
                |c| match c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    true => c,
                    false => '_',
                },
            )
            .collect();
        let hash = hash(&[command, &meta.path, &meta.len, &meta.mtime]);
        Some(dir.join(format!("{safe}-{hash:016x}.json")))
    }

    fn read_cache(&self, command: &str, meta: &Meta) -> Option<Option<Arc<Spec>>> {
        let path = self.cache_path(command, meta)?;
        let text = std::fs::read_to_string(path).ok()?;
        let cached: Cached = serde_json_lenient::from_str(&text).ok()?;
        if cached.version != VERSION
            || cached.command != command
            || cached.path != meta.path
            || cached.len != meta.len
            || cached.mtime != meta.mtime
        {
            return None;
        }
        Some(cached.spec.map(Arc::new))
    }

    fn write_cache(&self, command: &str, meta: &Meta, spec: &Option<Arc<Spec>>) {
        let Some(path) = self.cache_path(command, meta) else {
            return;
        };
        let cached = Cached {
            version: VERSION,
            command: command.to_string(),
            path: meta.path.clone(),
            len: meta.len.clone(),
            mtime: meta.mtime.clone(),
            spec: spec.as_ref().map(|spec| (**spec).clone()),
        };
        let Ok(json) = serde_json_lenient::to_string(&cached) else {
            return;
        };
        let Some(dir) = path.parent() else {
            return;
        };
        std::fs::create_dir_all(dir).ok();
        let temp = dir.join(format!(
            ".{}.{}.tmp",
            std::process::id(),
            path.file_name().unwrap().to_string_lossy()
        ));
        if std::fs::write(&temp, json).is_ok() {
            std::fs::rename(&temp, &path).ok();
        }
        std::fs::remove_file(&temp).ok();
    }
}

/// the identity of an installed program: a version or a rebuild changes its metadata and
/// so the cache key
#[derive(Clone)]
struct Meta {
    path: String,
    len: String,
    mtime: String,
}

impl Meta {
    fn read(program: &Path) -> Meta {
        let path = std::fs::canonicalize(program)
            .unwrap_or_else(|_| program.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let metadata = std::fs::metadata(program).ok();
        let len = metadata.as_ref().map_or(0, |meta| meta.len()).to_string();
        let mtime = metadata
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos())
            .to_string();
        Meta { path, len, mtime }
    }
}

#[derive(Serialize, Deserialize)]
struct Cached {
    version: u32,
    command: String,
    path: String,
    len: String,
    mtime: String,
    spec: Option<Spec>,
}

/// fnv 1a, so a cache file name is the same across builds and processes
fn hash(parts: &[&str]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for part in parts {
        for byte in part.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("compcore-probe-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn program(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const HELP: &str = "#!/bin/sh
echo \"$@\" >> \"$(dirname \"$0\")/calls\"
case \"$1\" in
  --help|'') printf 'Usage: fake [OPTIONS] <COMMAND>\\n\\nCommands:\\n  go   Go somewhere\\n\\nOptions:\\n  --fast   Run fast\\n' ;;
  go) printf 'Usage: fake go\\n\\nOptions:\\n  --quick   Go quickly\\n' ;;
esac
";

    fn probe(path: &Path) -> Option<Arc<Spec>> {
        Prober::new(None).spec("fake", path, None)
    }

    #[test]
    fn help_and_subcommand_help_are_merged() {
        let dir = temp("merge");
        let path = program(&dir, HELP);
        let spec = probe(&path).unwrap();
        assert_eq!(spec.name.all()[0], "fake");
        assert!(spec.subcommands.iter().any(|sub| sub.name.contains("go")));
        let go = spec
            .subcommands
            .iter()
            .find(|sub| sub.name.contains("go"))
            .unwrap();
        assert!(
            go.options
                .iter()
                .any(|option| option.name.contains("--quick"))
        );
        // options of the command itself reach its subcommands
        assert!(spec.options.iter().all(|option| option.is_persistent));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_probe_uses_the_memory_and_disk_cache() {
        let dir = temp("cache");
        let path = program(&dir, HELP);
        let cache = dir.join("cache");
        let calls = || std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
        let prober = Prober::new(Some(cache.clone()));
        assert!(prober.spec("fake", &path, None).is_some());
        let first = calls();
        assert!(prober.spec("fake", &path, None).is_some());
        assert_eq!(calls(), first, "memory cache missed");
        // a new prober reads the disk cache instead of running the program again
        let fresh = Prober::new(Some(cache));
        assert!(fresh.spec("fake", &path, None).is_some());
        assert_eq!(calls(), first, "disk cache missed");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unusable_help_is_cached_as_nothing() {
        let dir = temp("negative");
        let path = program(&dir, "#!/bin/sh\necho run >> \"$(dirname \"$0\")/calls\"\n");
        let prober = Prober::new(Some(dir.join("cache")));
        assert!(prober.spec("fake", &path, None).is_none());
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(prober.spec("fake", &path, None).is_none());
        assert_eq!(std::fs::read_to_string(dir.join("calls")).unwrap(), calls);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_slow_program_gives_up_at_the_deadline() {
        let dir = temp("slow");
        let path = program(&dir, "#!/bin/sh\nsleep 10\n");
        let mut prober = Prober::new(None);
        prober.limits.timeout = Duration::from_millis(100);
        let started = std::time::Instant::now();
        assert!(prober.spec("fake", &path, None).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::remove_dir_all(&dir).ok();
    }
}
