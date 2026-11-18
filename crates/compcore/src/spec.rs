//! the bundled completion specs and the specs folder of the user. the spec model and parser
//! live in the shared compcore-spec crate, the same one the generator uses

use std::{
    borrow::Cow,
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

pub use compcore_spec::{Arg, Opt, Spec, Suggestion, parse};

use super::probe::Prober;

// a user spec bigger than this is a mistake, not a spec
const MAX_SPEC_BYTES: u64 = 8 << 20;

struct Bundled {
    name: &'static str,
    bytes: &'static [u8],
    gzip: bool,
}

macro_rules! spec {
    ($name:literal) => {
        Bundled {
            name: $name,
            bytes: include_bytes!(concat!("../../../specs/", $name, ".json")),
            gzip: false,
        }
    };
}

macro_rules! gz_spec {
    ($name:literal) => {
        Bundled {
            name: $name,
            bytes: include_bytes!(concat!("../../../specs/", $name, ".json.gz")),
            gzip: true,
        }
    };
}

/// specs compcore brings, by the command they are for. sh tools/generator/generate.sh makes
/// most of them, see docs/COMPLETIONS.md
const BUNDLED: &[Bundled] = &[
    spec!("7z"),
    spec!("apk"),
    spec!("apt-cache"),
    spec!("apt-get"),
    spec!("apt"),
    gz_spec!("aws"),
    spec!("basename"),
    spec!("brew"),
    spec!("bzip2"),
    spec!("cargo"),
    spec!("cat"),
    spec!("cd"),
    spec!("chgrp"),
    spec!("chmod"),
    spec!("chown"),
    spec!("claude"),
    spec!("cp"),
    spec!("crontab"),
    spec!("curl"),
    spec!("cut"),
    spec!("date"),
    spec!("df"),
    spec!("diff"),
    spec!("dirname"),
    spec!("dnf"),
    spec!("docker"),
    spec!("dpkg"),
    spec!("du"),
    spec!("fastfetch"),
    spec!("file"),
    spec!("find"),
    spec!("flatpak"),
    spec!("free"),
    spec!("git"),
    spec!("grep"),
    spec!("gunzip"),
    spec!("gzip"),
    spec!("head"),
    spec!("hostnamectl"),
    spec!("htop"),
    spec!("ip"),
    spec!("journalctl"),
    spec!("kill"),
    spec!("killall"),
    spec!("less"),
    spec!("llama-batched-bench"),
    spec!("llama-batched"),
    spec!("llama-bench"),
    spec!("llama-cli"),
    spec!("llama-completion"),
    spec!("llama-debug"),
    spec!("llama-diffusion-cli"),
    spec!("llama-embedding"),
    spec!("llama-eval-callback"),
    spec!("llama-finetune"),
    spec!("llama-fit-params"),
    spec!("llama-gguf-hash"),
    spec!("llama-gguf-split"),
    spec!("llama-gguf"),
    spec!("llama-idle"),
    spec!("llama-imatrix"),
    spec!("llama-lookahead"),
    spec!("llama-lookup-create"),
    spec!("llama-lookup-merge"),
    spec!("llama-lookup-stats"),
    spec!("llama-lookup"),
    spec!("llama-mtmd-cli"),
    spec!("llama-parallel"),
    spec!("llama-passkey"),
    spec!("llama-perplexity"),
    spec!("llama-quantize"),
    spec!("llama-results"),
    spec!("llama-retrieval"),
    spec!("llama-server"),
    spec!("llama-speculative-simple"),
    spec!("llama-speculative"),
    spec!("llama-tokenize"),
    spec!("llama-tts"),
    spec!("ln"),
    spec!("ls"),
    spec!("man"),
    spec!("mkdir"),
    spec!("mktemp"),
    spec!("mount"),
    spec!("mv"),
    spec!("npm"),
    spec!("obsidian-cli"),
    spec!("opencode"),
    spec!("pacman"),
    spec!("passwd"),
    spec!("ping"),
    spec!("pip"),
    spec!("pip3"),
    spec!("podman"),
    spec!("powerprofilesctl"),
    spec!("ps"),
    spec!("python"),
    spec!("python3"),
    spec!("realpath"),
    spec!("rm"),
    spec!("rmdir"),
    spec!("rpm"),
    spec!("rsync"),
    spec!("ruff"),
    spec!("scp"),
    spec!("sed"),
    spec!("snap"),
    spec!("sort"),
    spec!("ss"),
    spec!("ssh"),
    spec!("stat"),
    spec!("su"),
    spec!("systemctl"),
    spec!("tail"),
    spec!("tar"),
    spec!("tee"),
    spec!("timedatectl"),
    spec!("touch"),
    spec!("tr"),
    spec!("umount"),
    spec!("uname"),
    spec!("uniq"),
    spec!("unzip"),
    spec!("useradd"),
    spec!("userdel"),
    spec!("usermod"),
    spec!("uv"),
    spec!("wc"),
    spec!("wget"),
    spec!("which"),
    spec!("xz"),
    spec!("yum"),
    spec!("zip"),
    spec!("zstd"),
    spec!("zypper"),
];

/// a spec's json, parsed the first time it is asked for. the aws spec is gzipped, it is
/// megabytes of service and operation names otherwise
struct Entry {
    bytes: Cow<'static, [u8]>,
    gzip: bool,
    /// where the json is from, for errors
    origin: String,
    spec: OnceLock<Option<Arc<Spec>>>,
}

impl Entry {
    fn new(bytes: impl Into<Cow<'static, [u8]>>, gzip: bool, origin: String) -> Entry {
        Entry {
            bytes: bytes.into(),
            gzip,
            origin,
            spec: OnceLock::new(),
        }
    }

    fn json(&self) -> Result<Cow<'_, str>, String> {
        match self.gzip {
            false => Ok(String::from_utf8_lossy(&self.bytes)),
            true => {
                let mut json = String::new();
                flate2::read::GzDecoder::new(&self.bytes[..])
                    .read_to_string(&mut json)
                    .map_err(|error| format!("not gzip: {error}"))?;
                Ok(Cow::Owned(json))
            }
        }
    }

    // a broken spec says so once, then counts as missing
    fn spec(&self) -> Option<Arc<Spec>> {
        self.spec
            .get_or_init(|| {
                let result = self.json().and_then(|json| parse(&json));
                result
                    .inspect_err(|error| {
                        eprintln!("completion spec {} is broken: {error}", self.origin)
                    })
                    .ok()
                    .map(Arc::new)
            })
            .clone()
    }
}

/// specs by the command they are for, and the help probes of the commands without one
#[derive(Default)]
pub struct Specs {
    entries: HashMap<String, Entry>,
    prober: Prober,
}

impl Specs {
    /// the bundled specs, with every `<command>.json` of `dir` over them. each is only parsed
    /// when its command is completed, a broken one then shows an error on stderr. probed help
    /// is cached in `cache`, none keeps it in memory only
    pub fn load(dir: Option<&Path>, cache: Option<PathBuf>) -> Specs {
        let mut specs = Specs {
            prober: Prober::new(cache),
            ..Specs::default()
        };
        for bundled in BUNDLED {
            let origin = format!("{} bundled with compcore", bundled.name);
            let entry = Entry::new(bundled.bytes, bundled.gzip, origin);
            specs.entries.insert(bundled.name.to_string(), entry);
        }
        let Some(entries) = dir.and_then(|dir| std::fs::read_dir(dir).ok()) else {
            return specs;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let (Some(name), Some("json")) = (
                path.file_stem().and_then(|stem| stem.to_str()),
                path.extension().and_then(|ext| ext.to_str()),
            ) else {
                continue;
            };
            match read_small_file(&path) {
                Ok(json) => {
                    let entry = Entry::new(json.into_bytes(), false, path.display().to_string());
                    specs.entries.insert(name.to_string(), entry);
                }
                Err(error) => eprintln!("can't read {}: {error}", path.display()),
            }
        }
        specs
    }

    /// add a spec from json, as if it was a file of the specs folder
    pub fn insert(&mut self, name: &str, json: &'static str) {
        let entry = Entry::new(json.as_bytes(), false, name.to_string());
        self.entries.insert(name.to_string(), entry);
    }

    /// spec of the command `name`
    pub fn get(&self, name: &str) -> Option<Arc<Spec>> {
        self.entries.get(name)?.spec()
    }

    /// spec of an unknown command from its own help, cached in memory and on disk
    pub fn probe(&self, name: &str, path_var: &str, cwd: &Path) -> Option<Arc<Spec>> {
        let program = super::sources::program(name, path_var)?;
        self.prober.spec(name, &program, Some(cwd))
    }
}

/// the text of a file of at most `MAX_SPEC_BYTES`
fn read_small_file(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_SPEC_BYTES {
        return Err(std::io::Error::other(format!(
            "not a file of at most {} MB",
            MAX_SPEC_BYTES >> 20
        )));
    }
    let mut text = String::new();
    file.take(MAX_SPEC_BYTES).read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_spec_is_listed_and_parses() {
        let mut listed: Vec<String> = BUNDLED
            .iter()
            .map(|bundled| match bundled.gzip {
                true => format!("{}.json.gz", bundled.name),
                false => format!("{}.json", bundled.name),
            })
            .collect();
        listed.sort();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../specs");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(listed, files, "BUNDLED must list every file of {dir}");

        let specs = Specs::load(None, None);
        for bundled in BUNDLED {
            let name = bundled.name;
            let spec = specs
                .get(name)
                .unwrap_or_else(|| panic!("{name} doesn't parse"));
            assert!(spec.name.contains(name), "{name} is named {:?}", spec.name);
        }
    }

    #[test]
    fn the_aws_spec_has_its_services_inline() {
        let specs = Specs::load(None, None);
        let aws = specs.get("aws").unwrap();
        let ec2 = aws
            .subcommands
            .iter()
            .find(|s| s.name.contains("ec2"))
            .unwrap();
        let describe = ec2
            .subcommands
            .iter()
            .find(|s| s.name.contains("describe-instances"))
            .unwrap();
        assert!(
            describe
                .options
                .iter()
                .any(|o| o.name.contains("--instance-ids"))
        );
    }

    #[test]
    fn user_specs_go_over_bundled_ones_and_broken_ones_are_skipped() {
        let dir = std::env::temp_dir().join(format!("compcore-specs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = r#"{"name": "git", "description": "mine"}"#;
        std::fs::write(dir.join("git.json"), git).unwrap();
        std::fs::write(dir.join("new.json"), r#"{"name": "new"}"#).unwrap();
        std::fs::write(dir.join("bad.json"), "{ nope").unwrap();
        std::fs::write(dir.join("notes.txt"), r#"{"name": "notes"}"#).unwrap();
        let specs = Specs::load(Some(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
        let git = specs.get("git").unwrap();
        assert_eq!(git.description.as_deref(), Some("mine"));
        assert!(specs.get("new").is_some());
        assert!(specs.get("cargo").is_some());
        assert!(specs.get("bad").is_none());
        assert!(specs.get("notes").is_none());
        assert!(specs.get("missing/part").is_none());
    }

    #[test]
    fn a_folder_named_like_a_spec_is_skipped() {
        let dir = std::env::temp_dir().join(format!("compcore-specs-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder.json")).unwrap();
        std::fs::write(dir.join("fine.json"), r#"{"name": "fine"}"#).unwrap();
        let specs = Specs::load(Some(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
        assert!(specs.get("folder").is_none());
        assert!(specs.get("fine").is_some());
    }
}
