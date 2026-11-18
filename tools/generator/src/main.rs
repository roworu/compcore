//! generates the bundled completion specs from the programs' own help. the same parser the
//! app uses at runtime, run inside a container that has the programs installed. see
//! docs/COMPLETIONS.md
//!
//!   compcore-gen --images [--manifest PATH]
//!   compcore-gen --group NAME [--manifest PATH] [--out DIR] [--overlays DIR] [command...]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use compcore_spec::{
    Args, Opt, Spec,
    help::{self, Hints},
    merge_child,
    runner::{self, Limits},
};
use serde::Deserialize;
use serde_json_lenient::Value;

const USAGE: &str = "usage: compcore-gen --images | --group NAME [options] [command...]
  --images        print the manifest's groups: key, image and install command, tab separated
  --group NAME    generate the specs of a group
  --manifest PATH defaults to /gen/manifest.json
  --out DIR       defaults to /out
  --overlays DIR  defaults to /gen/overlays";

// a program's help may take its time, a command list of a big cli can be long
const LIMITS: Limits = Limits {
    timeout: Duration::from_secs(30),
    max_bytes: 4 * 1024 * 1024,
};
// help runs one command may take while filling its subcommands
const RUN_BUDGET: usize = 400;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut manifest_path = PathBuf::from("/gen/manifest.json");
    let mut out = PathBuf::from("/out");
    let mut overlays = PathBuf::from("/gen/overlays");
    let mut group = None;
    let mut images = false;
    let mut selected = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |what: &str| {
            args.next()
                .ok_or_else(|| format!("{what} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--images" => images = true,
            "--manifest" => manifest_path = PathBuf::from(value("--manifest")?),
            "--out" => out = PathBuf::from(value("--out")?),
            "--overlays" => overlays = PathBuf::from(value("--overlays")?),
            "--group" => group = Some(value("--group")?),
            "-h" | "--help" => return Err(USAGE.into()),
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n{USAGE}"));
            }
            command => selected.push(command.to_string()),
        }
    }
    let manifest = read_manifest(&manifest_path)?;
    if images {
        let mut groups: Vec<_> = manifest.groups.iter().collect();
        groups.sort_by_key(|(name, _)| *name);
        for (name, group) in groups {
            println!("{name}\t{}\t{}", group.image, group.install);
        }
        return Ok(());
    }
    let name = group.ok_or_else(|| USAGE.to_string())?;
    let group = manifest.groups.get(&name).ok_or_else(|| {
        let mut known: Vec<_> = manifest.groups.keys().cloned().collect();
        known.sort();
        format!("unknown group {name}, one of {}\n{USAGE}", known.join(", "))
    })?;
    generate(group, &selected, &out, &overlays)
}

#[derive(Deserialize)]
struct Manifest {
    groups: HashMap<String, Group>,
}

#[derive(Deserialize)]
struct Group {
    image: String,
    install: String,
    #[serde(default)]
    commands: Vec<CommandEntry>,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum CommandEntry {
    Name(String),
    Full(Box<Command>),
}

impl CommandEntry {
    fn command(self) -> Command {
        match self {
            CommandEntry::Name(name) => Command {
                name,
                ..Command::default()
            },
            CommandEntry::Full(command) => *command,
        }
    }
}

/// one program of a group and what its help needs
#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Command {
    name: String,
    description: Option<String>,
    /// arguments that print the program's help
    help: Vec<String>,
    /// arguments that print a subcommand's help, `{}` stands for its path (so a recipe can
    /// use `["help", "{}"]`); none disables recursion
    sub_help: Option<Vec<String>>,
    /// levels of subcommands to ask for help, 0 keeps only what the top help lists
    depth: usize,
    /// section titles that list commands, none means titles mentioning commands
    sections: Option<Vec<String>>,
    /// the top options are also taken after subcommands
    persistent: bool,
    /// subcommands listed first, in this order
    top: Vec<String>,
    /// what the program takes, overriding the usage line
    args: Option<Args>,
    /// arguments of subcommands by their path, like `install` or `pip install`
    sub_args: HashMap<String, Args>,
    /// a json file merged over the generated spec, relative to the overlays folder
    overlay: Option<String>,
    /// the spec is written by hand and regeneration leaves it alone
    handwritten: bool,
    /// more invocations whose help is merged into the top spec, like pacman's operations
    extra_help: Vec<Vec<String>>,
    extra_options: Vec<Opt>,
    extra_subcommands: Vec<Spec>,
}

impl Default for Command {
    fn default() -> Self {
        Command {
            name: String::new(),
            description: None,
            help: vec!["--help".into()],
            sub_help: Some(vec!["{}".into(), "--help".into()]),
            depth: 2,
            sections: None,
            persistent: false,
            top: Vec::new(),
            args: None,
            sub_args: HashMap::new(),
            overlay: None,
            handwritten: false,
            extra_help: Vec::new(),
            extra_options: Vec::new(),
            extra_subcommands: Vec::new(),
        }
    }
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("can't read {}: {error}", path.display()))?;
    serde_json_lenient::from_str(&text)
        .map_err(|error| format!("{} is not a valid manifest: {error}", path.display()))
}

fn generate(group: &Group, selected: &[String], out: &Path, overlays: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out)
        .map_err(|error| format!("can't create {}: {error}", out.display()))?;
    let mut failed = Vec::new();
    for entry in &group.commands {
        let command = entry.clone().command();
        if command.handwritten || command.name.is_empty() {
            continue;
        }
        if !selected.is_empty() && !selected.contains(&command.name) {
            continue;
        }
        match build(&command, overlays) {
            Ok(spec) => {
                let path = out.join(format!("{}.json", command.name));
                let json = serde_json_lenient::to_string_pretty(&spec)
                    .map_err(|error| format!("{}: {error}", command.name))?;
                std::fs::write(&path, json + "\n")
                    .map_err(|error| format!("can't write {}: {error}", path.display()))?;
                println!(
                    "{}: {} subcommands, {} options",
                    path.display(),
                    spec.subcommands.len(),
                    spec.options.len()
                );
            }
            Err(error) => {
                eprintln!("{}: {error}", command.name);
                failed.push(command.name);
            }
        }
    }
    match failed.is_empty() {
        true => Ok(()),
        false => Err(format!("no usable help for: {}", failed.join(", "))),
    }
}

/// a spec from the program's help, filled and overlaid
fn build(command: &Command, overlays: &Path) -> Result<Spec, String> {
    let hints = Hints {
        sections: command.sections.clone(),
    };
    let text = help_text(&command.name, &command.help);
    if text.trim().is_empty() {
        return Err("printed no help".into());
    }
    let mut spec = help::parse_help_with(&command.name, &text, &hints);
    if let Some(description) = &command.description {
        spec.description = Some(description.clone());
    }
    if command.persistent {
        for option in &mut spec.options {
            option.is_persistent = true;
        }
    }
    if let Some(args) = &command.args {
        spec.args = args.clone();
    }
    priorities(&mut spec.subcommands, &command.top);
    for args in &command.extra_help {
        let text = help_text(&command.name, args);
        if text.trim().is_empty() {
            continue;
        }
        let extra = help::parse_help_with(&command.name, &text, &hints);
        merge_extra(&mut spec, extra);
    }
    if command.depth > 0
        && let Some(template) = &command.sub_help
    {
        let mut budget = RUN_BUDGET;
        fill(
            &mut spec,
            &command.name,
            &[],
            template,
            &hints,
            command.depth,
            &mut budget,
        );
    }
    apply_sub_args(&mut spec, &command.sub_args, "");
    spec.options.extend(command.extra_options.clone());
    spec.subcommands.extend(command.extra_subcommands.clone());
    if let Some(overlay) = &command.overlay {
        apply_overlay(&mut spec, &overlays.join(overlay))?;
    }
    match spec.usable() {
        true => Ok(spec),
        false => Err("help lists no options, subcommands or arguments".into()),
    }
}

fn help_text(program: &str, args: &[String]) -> String {
    runner::run_help(Path::new(program), args, None, LIMITS)
}

fn priorities(subcommands: &mut [Spec], top: &[String]) {
    for sub in subcommands {
        if let Some(ix) = top.iter().position(|name| sub.name.contains(name)) {
            sub.priority = Some(76u8.saturating_sub((ix / 4) as u8));
        }
    }
}

fn apply_sub_args(spec: &mut Spec, map: &HashMap<String, Args>, prefix: &str) {
    for sub in &mut spec.subcommands {
        let name = sub.name.all()[0].clone();
        let path = match prefix.is_empty() {
            true => name,
            false => format!("{prefix} {name}"),
        };
        if let Some(args) = map.get(&path) {
            sub.args = args.clone();
        }
        apply_sub_args(sub, map, &path);
    }
}

/// ask every subcommand for its own help, `depth` levels down
fn fill(
    spec: &mut Spec,
    program: &str,
    prefix: &[String],
    template: &[String],
    hints: &Hints,
    depth: usize,
    budget: &mut usize,
) {
    if depth == 0 {
        return;
    }
    let subcommands = std::mem::take(&mut spec.subcommands);
    for mut sub in subcommands {
        if *budget == 0 {
            spec.subcommands.push(sub);
            continue;
        }
        let name = sub.name.all()[0].clone();
        let mut path = prefix.to_vec();
        path.push(name);
        let args: Vec<String> = template
            .iter()
            .flat_map(|part| match part.as_str() {
                "{}" => path.clone(),
                _ => vec![part.clone()],
            })
            .collect();
        *budget -= 1;
        let text = help_text(program, &args);
        if !text.trim().is_empty() {
            // yargs prints the whole path before each command, the parser strips it
            let parsed = help::parse_help_as(path.last().unwrap(), &text, &path.join(" "), hints);
            merge_child(&mut sub, parsed);
            fill(&mut sub, program, &path, template, hints, depth - 1, budget);
        }
        spec.subcommands.push(sub);
    }
}

fn merge_extra(spec: &mut Spec, extra: Spec) {
    for option in extra.options {
        let known = option
            .name
            .all()
            .iter()
            .any(|name| spec.options.iter().any(|have| have.name.contains(name)));
        if !known {
            spec.options.push(option);
        }
    }
    for sub in extra.subcommands {
        let known = sub
            .name
            .all()
            .iter()
            .any(|name| spec.subcommands.iter().any(|have| have.name.contains(name)));
        if !known {
            spec.subcommands.push(sub);
        }
    }
    if spec.args.all().is_empty() {
        spec.args = extra.args;
    }
}

fn apply_overlay(spec: &mut Spec, path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("can't read {}: {error}", path.display()))?;
    let overlay: Value = serde_json_lenient::from_str(&text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut base = serde_json_lenient::to_value(spec.clone())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    merge_value(&mut base, &overlay);
    *spec = serde_json_lenient::from_value(base)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(())
}

/// an overlay wins over the generated spec. objects merge, arrays of named items merge by
/// name (a null drops one), other arrays replace
fn merge_value(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(key) {
                    Some(slot) => merge_value(slot, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (Value::Array(base), Value::Array(overlay)) if named(base) && named(overlay) => {
            merge_named(base, overlay);
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

fn named(items: &[Value]) -> bool {
    !items.is_empty()
        && items
            .iter()
            .all(|item| item.is_null() || item.get("name").is_some())
}

fn merge_named(base: &mut Vec<Value>, overlay: &[Value]) {
    for item in overlay {
        let names = names_of(item);
        let known = base
            .iter()
            .position(|have| names_of(have).iter().any(|name| names.contains(name)));
        match (known, item.is_null()) {
            (Some(ix), true) => {
                base.remove(ix);
            }
            (Some(ix), false) => merge_value(&mut base[ix], item),
            (None, false) => base.push(item.clone()),
            (None, true) => {}
        }
    }
}

fn names_of(value: &Value) -> Vec<String> {
    match value.get("name") {
        Some(Value::String(name)) => vec![name.clone()],
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_merge_by_name_and_null_drops() {
        let mut base: Value = serde_json_lenient::from_str(
            r#"{"name":"t","options":[{"name":"-a","description":"a"},{"name":"-b"}],
                "args":[{"name":"one"}]}"#,
        )
        .unwrap();
        let overlay: Value = serde_json_lenient::from_str(
            r#"{"description":"mine","options":[{"name":"-b","description":"b"},null,
                {"name":"-c"}],"args":[{"name":"two"}]}"#,
        )
        .unwrap();
        merge_value(&mut base, &overlay);
        assert_eq!(base["description"], "mine");
        // -b got its description, and the null entry removed -a
        assert_eq!(base["options"].as_array().unwrap().len(), 2);
        assert_eq!(base["options"][0]["name"], "-b");
        assert_eq!(base["options"][0]["description"], "b");
        assert_eq!(base["options"][1]["name"], "-c");
        // plain arrays replace whole
        assert_eq!(base["args"].as_array().unwrap().len(), 1);
        assert_eq!(base["args"][0]["name"], "two");
    }

    #[test]
    fn sub_args_match_the_whole_path() {
        let mut spec: Spec = serde_json_lenient::from_str(
            r#"{"name":"t","subcommands":[{"name":"pip","subcommands":[{"name":"install"}]}]}"#,
        )
        .unwrap();
        let mut map = HashMap::new();
        map.insert(
            "pip install".to_string(),
            serde_json_lenient::from_str(r#"{"name":"package"}"#).unwrap(),
        );
        apply_sub_args(&mut spec, &map, "");
        let pip = &spec.subcommands[0];
        assert_eq!(
            pip.subcommands[0].args.at(0).unwrap().name.as_deref(),
            Some("package")
        );
    }
}
