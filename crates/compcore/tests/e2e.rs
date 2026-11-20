//! end-to-end tests of the `compcore` binary: its cli, the `serve` protocol as a frontend
//! speaks it, help probing with its disk cache, and latency and memory budgets. these run the
//! real binary, so they live here and not in a `mod tests`

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio},
    time::{Duration, Instant},
};

// budgets for a release build, a few times what a laptop takes so a busy ci runner passes,
// small enough that a slip like parsing every spec at start or per keystroke fails
const COLD_BUDGET: Duration = Duration::from_millis(300);
const FIRST_BUDGET: Duration = Duration::from_millis(300);
const WARM_MEDIAN_BUDGET: Duration = Duration::from_millis(2);
const WARM_MAX_BUDGET: Duration = Duration::from_millis(50);
const PEAK_MEMORY_BUDGET_KIB: u64 = 64 * 1024;

const BIN: &str = env!("CARGO_BIN_EXE_compcore");

/// a scratch folder with a git repository, a makefile, a few files and an empty `bin/`
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("compcore-e2e-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    for sub in ["bin", "home", "cache", "src", ".git/refs/heads"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(dir.join(".git/refs/heads/main"), "x\n").unwrap();
    std::fs::write(dir.join(".git/refs/heads/feature"), "x\n").unwrap();
    std::fs::write(dir.join("Makefile"), "build: ## build it\n\tcc x\ntest:\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "").unwrap();
    std::fs::write(dir.join("notes one.txt"), "").unwrap();
    dir
}

/// writes an executable script. a child forked by another test thread while this process
/// holds the file open for writing would make running it fail with "text file busy", so a
/// short-lived `sh` writes it instead
fn write_program(path: &Path, body: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

/// the binary with a clean environment rooted in `dir`
fn compcore(dir: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .current_dir(dir)
        .env_clear()
        .env("PATH", dir.join("bin"))
        .env("HOME", dir.join("home"))
        .env("XDG_CACHE_HOME", dir.join("cache"));
    command
}

fn run(dir: &Path, args: &[&str]) -> Output {
    compcore(dir).args(args).output().unwrap()
}

/// the labels `compcore complete` prints, the first field of each line
fn labels(output: &Output) -> Vec<String> {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.split('\t').next().unwrap().to_string())
        .collect()
}

#[test]
fn help_version_and_bad_usage() {
    let dir = fixture("cli");
    let version = run(&dir, &["--version"]);
    assert!(version.status.success());
    let expected = format!("compcore {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8_lossy(&version.stdout), expected);
    let help = run(&dir, &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("usage: compcore"));
    // usage goes to stderr with a failure for anything the binary can't do
    for args in [
        &[][..],
        &["nope"],
        &["complete"],
        &["serve", "line"],
        &["complete", "a", "b"],
    ] {
        let output = run(&dir, args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn complete_prints_one_candidate_per_line() {
    let dir = fixture("complete");
    let output = run(&dir, &["complete", "--no-probe", "git ch"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let checkout = stdout
        .lines()
        .find(|line| line.starts_with("checkout\t"))
        .unwrap();
    let fields: Vec<&str> = checkout.split('\t').collect();
    assert_eq!(fields[1..3], ["\"checkout \"", "subcommand"]);
    assert!(!fields[3].is_empty());
    // the cwd, HOME and the project's files are the process's own
    assert_eq!(
        labels(&run(&dir, &["complete", "cat no"])),
        ["notes one.txt"]
    );
    assert_eq!(
        labels(&run(&dir, &["complete", "ls ~/"])),
        Vec::<String>::new()
    );
    assert_eq!(
        labels(&run(&dir, &["complete", "git switch "])),
        ["feature", "main"]
    );
    assert_eq!(
        labels(&run(&dir, &["complete", "make "])),
        ["build", "test"]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn user_specs_go_over_bundled_ones() {
    let dir = fixture("specs");
    let specs = dir.join("specs");
    std::fs::create_dir_all(&specs).unwrap();
    std::fs::write(
        specs.join("git.json"),
        r#"{"name": "git", "subcommands": [{"name": "mine"}]}"#,
    )
    .unwrap();
    let specs = specs.to_str().unwrap();
    assert_eq!(
        labels(&run(&dir, &["complete", "--specs", specs, "git "])),
        ["mine"]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_commands_are_probed_once_and_cached_on_disk() {
    let dir = fixture("probe");
    write_program(
        &dir.join("bin/fake"),
        "#!/bin/sh\necho run >> \"${0%/*}/../calls\"\nprintf 'Usage: fake [OPTIONS]\\n\\nOptions:\\n  --fast   Run fast\\n'\n",
    );
    let calls = || std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
    assert!(labels(&run(&dir, &["complete", "--no-probe", "fake --f"])).is_empty());
    assert_eq!(calls(), "");
    assert_eq!(labels(&run(&dir, &["complete", "fake --f"])), ["--fast"]);
    let after_first = calls();
    assert!(!after_first.is_empty());
    assert!(
        std::fs::read_dir(dir.join("cache/compcore"))
            .unwrap()
            .count()
            > 0
    );
    // a new process answers from the disk cache without running the program again
    assert_eq!(labels(&run(&dir, &["complete", "fake --f"])), ["--fast"]);
    assert_eq!(calls(), after_first);
    std::fs::remove_dir_all(&dir).ok();
}

/// a running `compcore serve`, spoken to like a frontend
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    dir: PathBuf,
}

impl Server {
    fn start(dir: &Path) -> Server {
        let mut child = compcore(dir)
            .args(["serve", "--no-probe"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Server {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            dir: dir.to_path_buf(),
        }
    }

    fn send(&mut self, record: &str) {
        self.stdin.write_all(record.as_bytes()).unwrap();
        self.stdin.write_all(b"\0").unwrap();
        self.stdin.flush().unwrap();
    }

    fn reply(&mut self) -> String {
        let mut reply = Vec::new();
        self.stdout.read_until(0, &mut reply).unwrap();
        assert_eq!(reply.pop(), Some(0), "the reply ends with a nul");
        String::from_utf8(reply).unwrap()
    }

    /// the reply to `line` completed at its end, in the fixture with its `bin/` as PATH
    fn ask(&mut self, seq: u64, line: &str) -> String {
        let record = format!(
            "{seq}\x1f{}\x1f{}\x1f{}\x1f{line}",
            line.chars().count(),
            self.dir.display(),
            self.dir.join("bin").display(),
        );
        self.send(&record);
        self.reply()
    }

    /// waits for the end of the process once its input is closed
    fn finish(self) -> std::process::ExitStatus {
        let Server {
            mut child,
            stdin,
            mut stdout,
            ..
        } = self;
        drop(stdin);
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "no reply without a request");
        child.wait().unwrap()
    }
}

fn reply_labels(reply: &str) -> Vec<&str> {
    reply
        .split('\x1e')
        .skip(1)
        .map(|candidate| candidate.split('\x1f').next().unwrap())
        .collect()
}

#[test]
fn serve_answers_each_request_until_its_input_ends() {
    let dir = fixture("serve");
    let mut server = Server::start(&dir);
    let reply = server.ask(1, "git sw");
    let (head, candidates) = reply.split_once('\x1e').unwrap();
    assert_eq!(head, "1\x1f4\x1f6");
    let fields: Vec<&str> = candidates.split('\x1f').collect();
    assert_eq!(
        (fields[0], fields[1], fields[3]),
        ("switch", "switch ", "subcommand")
    );
    assert_eq!(reply_labels(&server.ask(2, "make t")), ["test"]);
    assert_eq!(
        reply_labels(&server.ask(3, "git switch ")),
        ["feature", "main"]
    );
    // a request that can't be completed still gets a reply, one without a seq gets none
    server.send("4\x1f0\x1frelative\x1f\x1fgit ");
    assert_eq!(server.reply(), "4\x1f0\x1f0");
    server.send("garbage");
    assert_eq!(reply_labels(&server.ask(5, "make b")), ["build"]);
    assert!(server.finish().success());
    std::fs::remove_dir_all(&dir).ok();
}

/// command lines across the bundled specs, files, PATH and the project's git and make
const WORKLOAD: &[&str] = &[
    "gi",
    "git ch",
    "git checkout ",
    "git commit --am",
    "git switch m",
    "aws ec2 describe-inst",
    "aws s3 ",
    "aws ec2 describe-instances --inst",
    "podman run --",
    "docker ",
    "make ",
    "ls -",
    "cat sr",
    "cd src/",
    "pacman -S fire",
    "sudo systemctl rest",
    "tar -",
    "cargo b",
];

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "budgets are for release builds, use --release"
)]
fn latency_and_memory_stay_in_budget() {
    let dir = fixture("perf");
    // a fresh process answering one line, aws's gzipped spec included
    let started = Instant::now();
    let cold = run(&dir, &["complete", "--no-probe", "aws ec2 describe-inst"]);
    let cold_time = started.elapsed();
    assert!(labels(&cold).contains(&"describe-instances".to_string()));

    let mut server = Server::start(&dir);
    // the first answer of each line loads its spec, later ones are what typing feels like
    let mut first = Vec::new();
    for (seq, line) in WORKLOAD.iter().enumerate() {
        let started = Instant::now();
        server.ask(seq as u64, line);
        first.push((started.elapsed(), *line));
    }
    let mut warm = Vec::new();
    for round in 0..20 {
        for (seq, line) in WORKLOAD.iter().enumerate() {
            let started = Instant::now();
            server.ask((round * 100 + seq) as u64, line);
            warm.push((started.elapsed(), *line));
        }
    }
    warm.sort();
    let median = warm[warm.len() / 2].0;
    let slowest = *warm.last().unwrap();
    let slowest_first = *first.iter().max().unwrap();
    let peak = peak_memory_kib(server.child.id());
    assert!(server.finish().success());
    eprintln!(
        "cold {cold_time:?}, first {slowest_first:?}, warm median {median:?} max {slowest:?}, \
         peak {peak:?} KiB"
    );
    assert!(cold_time < COLD_BUDGET, "cold start took {cold_time:?}");
    assert!(
        slowest_first.0 < FIRST_BUDGET,
        "first answer {slowest_first:?}"
    );
    assert!(median < WARM_MEDIAN_BUDGET, "warm median {median:?}");
    assert!(
        slowest.0 < WARM_MAX_BUDGET,
        "slowest warm answer {slowest:?}"
    );
    if let Some(peak) = peak {
        assert!(peak < PEAK_MEMORY_BUDGET_KIB, "peak memory {peak} KiB");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// the peak resident memory of a running process, linux only
fn peak_memory_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
