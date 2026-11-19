//! `compcore`: the completion engine for frontends that run it as a program
//!
//!   compcore serve [--specs DIR] [--no-probe]
//!   compcore complete [--specs DIR] [--no-probe] LINE
//!   compcore --help | --version

use std::{
    io::{BufReader, Write},
    path::PathBuf,
    process::ExitCode,
};

use compcore::{Request, Specs, complete, serve};

const USAGE: &str = "usage: compcore serve|complete [options] [LINE]
       compcore --help|--version
  serve        answer requests on stdin, see the serve module docs for the protocol
  complete     print the candidates for LINE, completed at its end, one per line
  --specs DIR  <command>.json specs that go over the bundled ones
  --no-probe   never run `<command> --help` for commands without a spec";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(mode) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    match mode.as_str() {
        "-h" | "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "-V" | "--version" => {
            println!("compcore {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let mut dir = None;
    let mut probe_help = true;
    let mut line = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--specs" => dir = args.next().map(PathBuf::from),
            "--no-probe" => probe_help = false,
            _ if line.is_none() && !arg.starts_with("--") => line = Some(arg),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let specs = Specs::load(dir.as_deref(), cache_dir());
    let result = match (mode.as_str(), line) {
        ("serve", None) => serve::run(
            BufReader::new(std::io::stdin()),
            std::io::stdout().lock(),
            &specs,
            probe_help,
        ),
        ("complete", Some(line)) => print(&line, &specs, probe_help),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("compcore: {error}");
            ExitCode::FAILURE
        }
    }
}

/// candidates of `line` as `label<TAB>insert<TAB>kind<TAB>description`, for a look by hand
fn print(line: &str, specs: &Specs, probe_help: bool) -> std::io::Result<()> {
    let request = Request {
        buffer: line.to_string(),
        cursor: line.chars().count(),
        cwd: std::env::current_dir()?,
        path_var: std::env::var("PATH").unwrap_or_default(),
        home: std::env::var_os("HOME").map(PathBuf::from),
        probe_help,
    };
    let mut out = std::io::stdout().lock();
    for candidate in complete(&request, specs).candidates {
        let description = candidate.description.unwrap_or_default();
        writeln!(
            out,
            "{}\t{:?}\t{}\t{description}",
            candidate.label,
            candidate.insert,
            candidate.kind.name()
        )?;
    }
    Ok(())
}

/// where probed help is cached, `$XDG_CACHE_HOME/compcore`
fn cache_dir() -> Option<PathBuf> {
    // xdg says empty or relative values must be ignored
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|dir| dir.join("compcore"))
}
