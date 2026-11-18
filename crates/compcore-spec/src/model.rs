//! the declarative completion spec, in the json form of fig's specs
//! (github.com/withfig/autocomplete). only the declarative parts are read, code valued fields
//! like generators and scripts are ignored, so a spec never runs anything

use serde::{Deserialize, Serialize};
use serde_json_lenient::Value;

/// one name or several that mean the same, like `["-v", "--verbose"]`
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Names {
    One(String),
    Many(Vec<String>),
}

impl Default for Names {
    fn default() -> Self {
        Names::Many(Vec::new())
    }
}

impl Names {
    pub fn all(&self) -> &[String] {
        match self {
            Names::One(name) => std::slice::from_ref(name),
            Names::Many(names) => names,
        }
    }

    pub fn contains(&self, word: &str) -> bool {
        self.all().iter().any(|name| name == word)
    }
}

/// a command or subcommand
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Spec {
    pub name: Names,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<Spec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Opt>,
    #[serde(skip_serializing_if = "args_empty")]
    pub args: Args,
    /// 0 to 100, higher is listed first, 50 when not set
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
}

impl Spec {
    /// true when help said anything worth completing
    pub fn usable(&self) -> bool {
        !(self.options.is_empty() && self.subcommands.is_empty() && self.args.all().is_empty())
    }
}

/// add what a deeper help text says to a subcommand its parent already listed, keeping the
/// parent's name, aliases, description and priority
pub fn merge_child(base: &mut Spec, extra: Spec) {
    if base.args.all().is_empty() && !extra.args.all().is_empty() {
        base.args = extra.args;
    }
    for option in extra.options {
        let known = option
            .name
            .all()
            .iter()
            .any(|name| base.options.iter().any(|have| have.name.contains(name)));
        if !known {
            base.options.push(option);
        }
    }
    if base.subcommands.is_empty() {
        base.subcommands = extra.subcommands;
    }
    if base.description.is_none() {
        base.description = extra.description;
    }
}

/// a flag like `--verbose`, maybe taking arguments
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Opt {
    pub name: Names,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "args_empty")]
    pub args: Args,
    /// subcommands take it too
    #[serde(skip_serializing_if = "is_false")]
    pub is_persistent: bool,
    /// true or how many times it may be given, once when not set
    #[serde(skip_serializing_if = "Value::is_null")]
    pub is_repeatable: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
}

impl Opt {
    pub fn repeatable(&self) -> bool {
        match &self.is_repeatable {
            Value::Bool(repeatable) => *repeatable,
            Value::Number(times) => times.as_u64().is_some_and(|times| times > 1),
            _ => false,
        }
    }
}

/// one argument or a list of them, in order
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Args {
    // first, a struct would also take a list as its fields in order
    Many(Vec<Arg>),
    One(Box<Arg>),
}

impl Default for Args {
    fn default() -> Self {
        Args::Many(Vec::new())
    }
}

impl Args {
    pub fn all(&self) -> &[Arg] {
        match self {
            Args::Many(args) => args,
            Args::One(arg) => std::slice::from_ref(&**arg),
        }
    }

    /// the argument at `ix`, the last one again when it takes any number
    pub fn at(&self, ix: usize) -> Option<&Arg> {
        let all = self.all();
        all.get(ix)
            .or_else(|| all.last().filter(|arg| arg.is_variadic))
    }
}

/// a positional argument
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Arg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// "filepaths", "folders" or both
    #[serde(skip_serializing_if = "templates_empty")]
    pub template: Templates,
    #[serde(skip_serializing_if = "is_false")]
    pub is_optional: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub is_variadic: bool,
}

/// a fixed value an argument can take
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Suggestion {
    Plain(String),
    Full {
        name: Names,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<u8>,
        #[serde(default, skip_serializing_if = "is_false")]
        hidden: bool,
    },
}

fn args_empty(args: &Args) -> bool {
    args.all().is_empty()
}

fn templates_empty(templates: &Templates) -> bool {
    match templates {
        Templates::One(one) => one.is_empty(),
        Templates::Many(many) => many.is_empty(),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// files an argument takes
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Templates {
    One(String),
    Many(Vec<String>),
}

impl Default for Templates {
    fn default() -> Self {
        Templates::Many(Vec::new())
    }
}

impl Templates {
    fn has(&self, template: &str) -> bool {
        match self {
            Templates::One(one) => one == template,
            Templates::Many(many) => many.iter().any(|one| one == template),
        }
    }

    pub fn files(&self) -> bool {
        self.has("filepaths")
    }

    /// folders only, or files too
    pub fn folders(&self) -> bool {
        self.has("folders") || self.files()
    }

    /// the package manager of a `packages:<manager>` template
    pub fn packages(&self) -> Option<&str> {
        let all = match self {
            Templates::One(one) => std::slice::from_ref(one),
            Templates::Many(many) => many.as_slice(),
        };
        all.iter().find_map(|one| one.strip_prefix("packages:"))
    }
}

/// a spec from json, rejecting specs without a name
pub fn parse(json: &str) -> Result<Spec, String> {
    let spec: Spec = serde_json_lenient::from_str(json).map_err(|error| error.to_string())?;
    check(&spec, "spec")?;
    Ok(spec)
}

// every command, subcommand and option needs a name to be typed by
fn check(spec: &Spec, at: &str) -> Result<(), String> {
    let named = |names: &Names| names.all().iter().any(|name| !name.is_empty());
    if !named(&spec.name) {
        return Err(format!("{at}: a command without a name"));
    }
    let at = &spec.name.all()[0];
    if let Some(option) = spec.options.iter().find(|option| !named(&option.name)) {
        return Err(format!("{at}: an option without a name: {option:?}"));
    }
    spec.subcommands.iter().try_for_each(|sub| check(sub, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fig_shapes_are_read() {
        let spec = parse(
            r#"{
                "name": "tool",
                "subcommands": [{ "name": ["remove", "rm"], "args": { "template": "filepaths", "isVariadic": true } }],
                "options": [
                    { "name": ["-v", "--verbose"], "isRepeatable": 3 },
                    { "name": "--out", "args": { "name": "file", "template": ["folders"] }, "isPersistent": true },
                ],
                "args": [{ "suggestions": ["a", { "name": ["b", "bee"], "description": "B" }] }, { "name": "n" }],
                // generators run commands, they are never read
                "generators": { "script": ["rm", "-rf", "/"] },
            }"#,
        )
        .unwrap();
        assert!(spec.subcommands[0].name.contains("rm"));
        assert!(spec.subcommands[0].args.at(5).unwrap().template.files());
        assert!(spec.options[0].repeatable());
        let out = &spec.options[1];
        assert!(out.is_persistent && out.args.at(0).unwrap().template.folders());
        assert!(!out.args.at(0).unwrap().template.files());
        assert_eq!(spec.args.all().len(), 2);
        assert!(spec.args.at(2).is_none());
        assert!(
            matches!(&spec.args.all()[0].suggestions[1], Suggestion::Full { name, .. } if name.contains("bee"))
        );
    }

    #[test]
    fn broken_specs_are_rejected() {
        assert!(parse("{").is_err());
        assert!(parse(r#"{"description": "no name"}"#).is_err());
        assert!(parse(r#"{"name": "a", "options": [{"description": "x"}]}"#).is_err());
        assert!(parse(r#"{"name": "a", "subcommands": [{"name": ""}]}"#).is_err());
        assert!(parse(r#"{"name": "a", "options": "nope"}"#).is_err());
    }

    #[test]
    fn is_repeatable_takes_a_bool_or_a_count() {
        let repeatable = |value: &str| {
            let json = format!(
                r#"{{"name": "t", "options": [{{"name": "-v", "isRepeatable": {value}}}]}}"#
            );
            parse(&json).unwrap().options[0].repeatable()
        };
        assert!(repeatable("true"));
        assert!(!repeatable("false"));
        // a count of one is the same as not repeatable
        assert!(!repeatable("1"));
        assert!(repeatable("2"));
        assert!(!repeatable(r#""often""#));
    }

    #[test]
    fn specs_round_trip_through_json() {
        let spec = parse(
            r#"{"name": "t", "description": "T", "subcommands": [{"name": ["go", "g"]}],
                "options": [{"name": ["-v", "--verbose"], "isRepeatable": true}],
                "args": {"name": "file", "template": "filepaths", "isVariadic": true}}"#,
        )
        .unwrap();
        let json = serde_json_lenient::to_string(&spec).unwrap();
        assert_eq!(parse(&json).unwrap(), spec);
    }
}
