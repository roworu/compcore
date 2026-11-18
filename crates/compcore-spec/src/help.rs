//! a program's --help text turned into a completion spec. one parser for every help dialect:
//! the shared shapes are clap/yargs columns, GNU two column rows, apt's `name - description`
//! and llama.cpp's fixed name column, all read by the same layout rules below

use crate::model::{Arg, Args, Names, Opt, Spec, Suggestion, Templates};
use serde_json_lenient::Value;

/// how to read the sections of a help text that doesn't title its command lists plainly
#[derive(Clone, Debug, Default)]
pub struct Hints {
    /// lowercased words a section title must contain, `*` for any section. none means
    /// titles mentioning commands, the shape of clap, cobra and argparse help
    pub sections: Option<Vec<String>>,
}

impl Hints {
    fn is_command_section(&self, section: &str) -> bool {
        let section = section.to_lowercase();
        if [
            "option",
            "example",
            "environment",
            "see also",
            "file",
            "author",
            "report",
            "usage",
        ]
        .iter()
        .any(|word| section.contains(word))
        {
            return false;
        }
        match &self.sections {
            Some(words) => words
                .iter()
                .any(|word| word == "*" || section.contains(&word.to_lowercase())),
            None => section.contains("command") || section.contains("subcommand"),
        }
    }
}

/// what a program says about itself in its help text
pub fn parse_help(command: &str, help: &str) -> Spec {
    parse_help_with(command, help, &Hints::default())
}

/// `parse_help` with the section hints a recipe gives for an unusual help layout
pub fn parse_help_with(command: &str, help: &str, hints: &Hints) -> Spec {
    parse_help_as(command, help, command, hints)
}

/// `parse_help` for a subcommand's help: the spec is named `name`, while `prefix` is the
/// command path the help prints before its command rows (yargs prints `opencode auth login`)
pub fn parse_help_as(name: &str, help: &str, prefix: &str, hints: &Hints) -> Spec {
    let lines = expand_columns(&strip_ansi(help));
    let mut spec = Spec {
        name: Names::One(name.to_string()),
        description: summary(&lines),
        ..Default::default()
    };
    let mut section = String::new();
    let mut options: Vec<Opt> = Vec::new();
    let mut seen_options = std::collections::HashSet::new();
    let mut seen_commands = std::collections::HashSet::new();
    let mut ix = 0;
    while ix < lines.len() {
        let line = &lines[ix];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            ix += 1;
            continue;
        }
        if let Some(header) = header_text(line) {
            section = header.to_lowercase();
            ix += 1;
            continue;
        }
        if hints.is_command_section(&section)
            && let Some((sub, consumed)) = parse_subcommand(prefix, line, &lines, ix)
        {
            let first = sub.name.all()[0].clone();
            if seen_commands.insert(first) {
                spec.subcommands.push(sub);
            }
            ix += consumed + 1;
            continue;
        }
        if trimmed.starts_with('-')
            && display_indent(line) <= 12
            && let Some((option, consumed)) = parse_option(line, &lines, ix)
        {
            let names: Vec<String> = option.name.all().to_vec();
            if names.iter().any(|name| !seen_options.contains(name)) {
                seen_options.extend(names);
                options.push(option);
            }
            ix += consumed + 1;
            continue;
        }
        ix += 1;
    }
    spec.options = options;
    spec.args = positional_args(&lines);
    spec
}

/// the positional arguments a help text lists, from an `Arguments:` section or the usage line
fn positional_args(lines: &[String]) -> Args {
    let mut section_args = Vec::new();
    let mut in_arguments = false;
    for line in lines {
        if let Some(header) = header_text(line) {
            in_arguments = header.to_lowercase().contains("argument");
            continue;
        }
        if !in_arguments {
            continue;
        }
        let (indent, head, _, description) = split_row(line);
        if indent == 0 {
            continue;
        }
        let Some(name) = head.split_whitespace().next() else {
            continue;
        };
        if name.starts_with('-') {
            continue;
        }
        let mut arg = arg_from(name, name.starts_with('['), name.ends_with("..."));
        arg.description = sentence(&strip_annotations(description));
        section_args.push(arg);
    }
    if !section_args.is_empty() {
        return Args::Many(section_args);
    }
    let usage = lines
        .iter()
        .find(|line| line.trim().to_lowercase().starts_with("usage:"))
        .map(|line| match line.split_once(':') {
            Some((_, rest)) => rest,
            None => "",
        });
    Args::Many(usage.map(usage_args).unwrap_or_default())
}

fn usage_args(usage: &str) -> Vec<Arg> {
    let mut words = usage.split_whitespace();
    words.next();
    let mut args = Vec::new();
    for word in words {
        let optional = word.starts_with('[');
        let mut name = word.trim_start_matches(['[', '<', '(', '{']);
        name = name.trim_end_matches([']', '>', ')', '}', ',', ':', '|']);
        let variadic = name.ends_with("...");
        name = name
            .trim_end_matches("...")
            .trim_end_matches([']', '>', ')', '}']);
        let lower = name.to_lowercase();
        if name.is_empty()
            || name.starts_with('-')
            || name.contains('|')
            || matches!(
                lower.as_str(),
                "option" | "options" | "command" | "commands" | "flags" | "args"
            )
            || !name.chars().next().is_some_and(|c| c.is_alphabetic())
        {
            continue;
        }
        args.push(arg_from(name, optional, variadic));
    }
    args
}

/// help printed by tools that underline section headers and color their output
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // a csi sequence ends at the first letter
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // osc sequences, like the hyperlinks some tools wrap names in, end at bel or st
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // other escapes are a two character sequence
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// a line with two options in columns, like zip's help, split into one option per line
fn expand_columns(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let bytes = line.as_bytes();
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let mut split = None;
        let mut ix = 0;
        while ix < bytes.len() {
            if bytes[ix] != b' ' {
                ix += 1;
                continue;
            }
            let mut end = ix;
            while end < bytes.len() && bytes[end] == b' ' {
                end += 1;
            }
            if end - ix >= 2
                && end < bytes.len()
                && bytes[end] == b'-'
                && line[..ix].trim_start().starts_with('-')
            {
                split = Some((ix, end));
                break;
            }
            ix = end;
        }
        if let Some((at, end)) = split {
            out.push(line[..at].trim_end().to_string());
            out.push(format!("{}{}", &line[..indent], &line[end..]));
        } else {
            out.push(line.to_string());
        }
    }
    out
}

fn display_indent(line: &str) -> usize {
    let trimmed = line.trim_start_matches([' ', '\t']);
    column(&line[..line.len() - trimmed.len()])
}

/// display width of `text`, with tabs at the usual 8 column stops
fn column(text: &str) -> usize {
    let mut column = 0;
    for c in text.chars() {
        match c {
            '\t' => column = (column / 8 + 1) * 8,
            _ => column += 1,
        }
    }
    column
}

/// a section title like `Commands:` or `Global options:`
fn header_text(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let title = trimmed.strip_suffix(':')?;
    (display_indent(line) <= 4 && !title.starts_with('-') && title.chars().any(char::is_alphabetic))
        .then_some(title.trim())
}

/// the first sentence a help text says about the command itself
fn summary(lines: &[String]) -> Option<String> {
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if header_text(line).is_some() {
            return None;
        }
        if trimmed.to_lowercase().starts_with("usage") {
            continue;
        }
        if trimmed.starts_with('-') || display_indent(line) > 0 {
            continue;
        }
        if let Some(sentence) = sentence(trimmed) {
            return Some(sentence);
        }
    }
    None
}

/// the name, description and description column of an indented row
fn split_row(line: &str) -> (usize, &str, Option<usize>, &str) {
    let indent = display_indent(line);
    let start = line.len() - line.trim_start_matches([' ', '\t']).len();
    let stripped = &line[start..];
    let bytes = stripped.as_bytes();
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'\t' {
            return (
                indent,
                stripped[..ix].trim_end(),
                Some(column(&line[..start + ix + 1])),
                stripped[ix..].trim_start_matches([' ', '\t']),
            );
        }
        if bytes[ix] != b' ' {
            ix += 1;
            continue;
        }
        let mut end = ix;
        while end < bytes.len() && bytes[end] == b' ' {
            end += 1;
        }
        if end - ix >= 2 {
            return (
                indent,
                stripped[..ix].trim_end(),
                Some(column(&line[..start + end])),
                &stripped[end..],
            );
        }
        ix = end;
    }
    (indent, stripped.trim_end(), None, "")
}

/// the description lines that wrap below a row, and how many they are
fn continuation(
    lines: &[String],
    ix: usize,
    description_column: Option<usize>,
    indent: usize,
) -> (String, usize) {
    let mut text = String::new();
    let mut consumed = 0;
    for line in &lines[ix + 1..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        let line_indent = display_indent(line);
        if header_text(line).is_some() {
            break;
        }
        let wraps = match description_column {
            Some(column) => line_indent >= column,
            None => line_indent > indent && !trimmed.starts_with('-'),
        };
        if !wraps {
            break;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(trimmed);
        consumed += 1;
    }
    (text, consumed)
}

fn parse_option(line: &str, lines: &[String], ix: usize) -> Option<(Opt, usize)> {
    let (indent, head, description_column, description) = split_row(line);
    let head = head.trim();
    if !head.starts_with('-') {
        return None;
    }
    let (names, attached, repeatable) = parse_names(head);
    if names.is_empty() {
        return None;
    }
    let (wrapped, consumed) = continuation(lines, ix, description_column, indent);
    let description = match wrapped.is_empty() {
        true => description.to_string(),
        false => format!("{description} {wrapped}"),
    };
    let mut option = Opt {
        name: match names.as_slice() {
            [one] => Names::One(one.clone()),
            many => Names::Many(many.to_vec()),
        },
        description: sentence(&strip_annotations(&description)),
        is_repeatable: match repeatable {
            true => Value::Bool(true),
            false => Value::Null,
        },
        ..Default::default()
    };
    let values = choices(&format!("{head} {description}"));
    if let Some(attached) = attached {
        let mut arg = arg_from(&attached.raw, attached.optional, attached.variadic);
        arg.suggestions = values;
        option.args = Args::One(Box::new(arg));
    } else if !values.is_empty() {
        option.args = Args::One(Box::new(Arg {
            name: Some("value".into()),
            suggestions: values,
            ..Default::default()
        }));
    } else if yargs_takes_value(&description) {
        let long = names
            .iter()
            .find(|name| name.starts_with("--"))
            .unwrap_or(&names[0]);
        let mut arg = Arg {
            name: Some(long.trim_start_matches('-').to_string()),
            ..Default::default()
        };
        if description.to_lowercase().contains("[array]") {
            arg.is_variadic = true;
        }
        option.args = Args::One(Box::new(arg));
    }
    Some((option, consumed))
}

#[derive(Default)]
struct Attached {
    raw: String,
    optional: bool,
    variadic: bool,
}

/// the names of an option and the argument written after them, like
/// `-f, --file=FILE`, `--color[=WHEN]`, `-o <file>` or `-M --machine=NAME`
fn parse_names(head: &str) -> (Vec<String>, Option<Attached>, bool) {
    let chars: Vec<char> = head.chars().collect();
    let mut names = Vec::new();
    let mut attached: Option<Attached> = None;
    let mut repeatable = false;
    let mut ix = 0;
    while ix < chars.len() {
        let next = chars.get(ix + 1).copied();
        let starts_name = |c: char| c.is_ascii_alphanumeric() || matches!(c, '?' | '#');
        let short = chars[ix] == '-'
            && (next.is_some_and(starts_name)
                || (next == Some('-') && chars.get(ix + 2).copied().is_some_and(starts_name)));
        if !short {
            ix += 1;
            continue;
        }
        let start = ix;
        ix += match next {
            Some('-') => 2,
            _ => 1,
        };
        while ix < chars.len() && is_name_char(chars[ix]) {
            ix += 1;
        }
        names.push(chars[start..ix].iter().collect());
        if chars.get(ix..ix + 3) == Some(&['.', '.', '.'][..]) {
            repeatable = true;
            ix += 3;
        }
        match chars.get(ix).copied() {
            Some('=') => {
                ix += 1;
                let start = ix;
                while ix < chars.len()
                    && !chars[ix].is_whitespace()
                    && !matches!(chars[ix], ',' | '|' | ']' | '>')
                {
                    ix += 1;
                }
                set_attached(&mut attached, chars[start..ix].iter().collect(), false);
            }
            Some('[') => {
                if let Some(close) = matching(&chars, ix, '[', ']') {
                    let inner: String = chars[ix + 1..close].iter().collect();
                    let (optional, value) = match inner.strip_prefix('=') {
                        Some(value) => (true, value.to_string()),
                        None => (true, inner),
                    };
                    set_attached(&mut attached, value, optional);
                    ix = close + 1;
                }
            }
            Some('<') => {
                if let Some((end, value)) = delimited(&chars, ix, '<', '>') {
                    set_attached(&mut attached, value, false);
                    ix = end;
                }
            }
            Some(c) if c.is_whitespace() => {
                let mut next = ix;
                while next < chars.len() && chars[next].is_whitespace() {
                    next += 1;
                }
                match chars.get(next).copied() {
                    Some('<') => {
                        if let Some((end, value)) = delimited(&chars, next, '<', '>') {
                            set_attached(&mut attached, value, false);
                            ix = end;
                        }
                    }
                    Some('[') => {
                        if let Some(close) = matching(&chars, next, '[', ']') {
                            let inner: String = chars[next + 1..close].iter().collect();
                            let (optional, value) = match inner.strip_prefix('=') {
                                Some(value) => (true, value.to_string()),
                                None => (true, inner),
                            };
                            set_attached(&mut attached, value, optional);
                            ix = close + 1;
                        }
                    }
                    Some(c) if c != '-' && c != ',' && !c.is_whitespace() => {
                        let start = next;
                        let mut end = next;
                        while end < chars.len()
                            && !chars[end].is_whitespace()
                            && !matches!(chars[end], ',' | '|' | ']' | '>')
                        {
                            end += 1;
                        }
                        let mut value: String = chars[start..end].iter().collect();
                        if let Some(stripped) = value.strip_suffix("...") {
                            value = stripped.to_string();
                            if let Some(attached) = &mut attached {
                                attached.variadic = true;
                            }
                        }
                        set_attached(&mut attached, value, false);
                        ix = end;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    (names, attached, repeatable)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '?' | '#' | '+' | '-' | '_')
}

fn matching(chars: &[char], at: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    for (ix, c) in chars.iter().enumerate().skip(at) {
        if *c == open {
            depth += 1;
        } else if *c == close {
            depth -= 1;
            if depth == 0 {
                return Some(ix);
            }
        }
    }
    None
}

/// the value between `open` and `close` and where the value ends, including `...`
fn delimited(chars: &[char], at: usize, open: char, close: char) -> Option<(usize, String)> {
    let end = matching(chars, at, open, close)?;
    let mut after = end + 1;
    let mut value: String = chars[at + 1..end].iter().collect();
    if chars.get(after..after + 3) == Some(&['.', '.', '.'][..]) {
        after += 3;
        value.push_str("...");
    }
    Some((after, value))
}

fn set_attached(attached: &mut Option<Attached>, raw: String, optional: bool) {
    if attached.is_none() && !raw.trim().is_empty() {
        *attached = Some(Attached {
            raw: raw.trim().to_string(),
            optional,
            variadic: raw.trim().ends_with("..."),
        });
    }
}

/// an argument from how help writes it, like `FILE`, `[=WHEN]` or `<dir>`
fn arg_from(raw: &str, optional: bool, variadic: bool) -> Arg {
    let raw = raw
        .trim()
        .trim_matches(|c| matches!(c, '[' | ']' | '<' | '>' | '='));
    let variadic = variadic || raw.ends_with("...");
    let name = raw
        .trim_end_matches("...")
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-')))
        .find(|part| !part.is_empty())
        .unwrap_or("value")
        .to_lowercase();
    let template = match is_folder_name(&name) {
        true => Templates::One("folders".into()),
        false if is_file_name(&name) => Templates::One("filepaths".into()),
        false => Templates::default(),
    };
    Arg {
        name: Some(name),
        template,
        is_optional: optional,
        is_variadic: variadic,
        ..Default::default()
    }
}

fn is_folder_name(name: &str) -> bool {
    matches!(name, "dir" | "directory" | "folder" | "dirs")
        || name.ends_with("dir")
        || name.ends_with("directory")
}

fn is_file_name(name: &str) -> bool {
    matches!(
        name,
        "file"
            | "filename"
            | "fname"
            | "file_name"
            | "path"
            | "files"
            | "archive"
            | "config"
            | "configfile"
            | "log"
            | "logfile"
            | "source"
            | "src"
            | "dest"
            | "destination"
            | "dst"
            | "target"
    ) || name.ends_with("_file")
        || name.ends_with("file")
}

/// yargs sort of help lists the type of an option's value in brackets
fn yargs_takes_value(description: &str) -> bool {
    let lower = description.to_lowercase();
    ["[string]", "[number]", "[array]"]
        .iter()
        .any(|kind| lower.contains(kind))
}

/// fixed values an option or argument can take, from the ways help lists them
fn choices(text: &str) -> Vec<Suggestion> {
    // ascii lowercasing keeps byte offsets, so the rest can be taken from the original
    let lower = text.to_ascii_lowercase();
    for label in ["possible values:", "allowed values:", "choices:"] {
        let Some(at) = lower.find(label) else {
            continue;
        };
        let rest = &text[at + label.len()..];
        let rest = rest.split(']').next().unwrap_or(rest);
        let values = split_values(rest);
        if !values.is_empty() {
            return values;
        }
    }
    let chars: Vec<char> = text.chars().collect();
    for (open, close) in [('<', '>'), ('{', '}'), ('[', ']')] {
        let Some(start) = chars.iter().position(|c| *c == open) else {
            continue;
        };
        let Some(end) = matching(&chars, start, open, close) else {
            continue;
        };
        let inner: String = chars[start + 1..end].iter().collect();
        if !(inner.contains('|') || (open == '{' && inner.contains(','))) {
            continue;
        }
        let values = split_values(&inner);
        if values.len() > 1 {
            return values;
        }
    }
    Vec::new()
}

fn split_values(text: &str) -> Vec<Suggestion> {
    text.split([',', '|'])
        .map(|value| {
            value
                .trim()
                .trim_matches(['"', '\'', ' ', '[', ']', '{', '}', '<', '>', '(', ')'])
        })
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .map(|value| Suggestion::Plain(value.to_string()))
        .collect()
}

/// remove what help adds for humans, like defaults and env var names, keeping the sentence
fn strip_annotations(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut ix = 0;
    while ix < chars.len() {
        let (open, close) = match chars[ix] {
            '[' => ('[', ']'),
            '(' => ('(', ')'),
            _ => {
                out.push(chars[ix]);
                ix += 1;
                continue;
            }
        };
        let Some(end) = matching(&chars, ix, open, close) else {
            out.push(chars[ix]);
            ix += 1;
            continue;
        };
        let inner: String = chars[ix + 1..end].iter().collect();
        let key = inner
            .trim()
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let drop = matches!(
            key.as_str(),
            "env"
                | "default"
                | "possible values"
                | "allowed values"
                | "choices"
                | "values"
                | "string"
                | "number"
                | "array"
                | "boolean"
                | "required"
                | "optional"
        );
        if drop {
            ix = end + 1;
        } else {
            out.extend(&chars[ix..=end]);
            ix = end + 1;
        }
    }
    out
}

/// the first sentence of a description, short enough to show next to a candidate
fn sentence(text: &str) -> Option<String> {
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text = text
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | ':' | ';' | ','))
        .to_string();
    if text.starts_with('(') && text.ends_with(')') && text.len() > 1 {
        text = text[1..text.len() - 1].to_string();
    }
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    for (ix, c) in chars.iter().enumerate() {
        let next = chars.get(ix + 1).copied();
        let next_word = chars[ix + 1..].iter().find(|c| !c.is_whitespace()).copied();
        if matches!(c, '!' | '?') && next.is_none_or(char::is_whitespace) {
            end = ix + 1;
            break;
        }
        if *c == '.'
            && next.is_some_and(char::is_whitespace)
            && next_word.is_some_and(|c| c.is_uppercase())
        {
            end = ix + 1;
            break;
        }
        if *c == ';' && next.is_some_and(char::is_whitespace) {
            end = ix;
            break;
        }
    }
    let mut out: String = chars[..end]
        .iter()
        .collect::<String>()
        .trim_end_matches([' ', '.', ',', ';', ':'])
        .to_string();
    if out.is_empty() {
        return None;
    }
    let mut first = out.chars();
    if let Some(c) = first.next() {
        out = c.to_uppercase().collect::<String>() + first.as_str();
    }
    Some(out)
}

/// a command row of a section, with the number of lines it used
fn parse_subcommand(
    command: &str,
    line: &str,
    lines: &[String],
    ix: usize,
) -> Option<(Spec, usize)> {
    let (indent, head, description_column, description) = split_row(line);
    if !(1..=8).contains(&indent) {
        return None;
    }
    let head = head.trim();
    // yargs prefixes command rows with the program name, but a command may share that name
    let head = match head
        .strip_prefix(command)
        .and_then(|rest| rest.strip_prefix(' '))
    {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_lowercase()) => rest,
        _ => head,
    };
    if head.starts_with('-') {
        return None;
    }
    // apt writes "install - Install packages" with single spaces
    let (head, description, description_column) = match description.is_empty() {
        true => match head.split_once(" - ") {
            Some((name, rest)) => (name.trim(), rest.trim(), None),
            None => (head, "", description_column),
        },
        false => (head, description, description_column),
    };
    let names = command_names(head)?;
    let (wrapped, consumed) = continuation(lines, ix, description_column, indent);
    let description = match wrapped.is_empty() {
        true => description.to_string(),
        false => format!("{description} {wrapped}"),
    };
    let sub = Spec {
        name: match names.as_slice() {
            [one] => Names::One(one.clone()),
            many => Names::Many(many.to_vec()),
        },
        description: sentence(&strip_annotations(&description)),
        ..Default::default()
    };
    Some((sub, consumed))
}

/// `name`, `name, alias` and `name ARGS` rows, without the arguments
fn command_names(head: &str) -> Option<Vec<String>> {
    let is_name =
        |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
    let mut chars = head.char_indices().peekable();
    let first_end = chars
        .find_map(|(ix, c)| (!is_name(c)).then_some(ix))
        .unwrap_or(head.len());
    if first_end == 0 || !head[..first_end].starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    let mut names = vec![head[..first_end].to_string()];
    let mut rest = &head[first_end..];
    loop {
        let trimmed = rest.trim_start();
        let Some(after) = trimmed.strip_prefix(',') else {
            break;
        };
        let after = after.trim_start();
        let end = after
            .char_indices()
            .find_map(|(ix, c)| (!is_name(c)).then_some(ix))
            .unwrap_or(after.len());
        if end == 0 {
            break;
        }
        names.push(after[..end].to_string());
        rest = &after[end..];
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Names {
        Names::Many(names.iter().map(|name| name.to_string()).collect())
    }

    #[test]
    fn clap_commands_and_options() {
        let spec = parse_help(
            "tool",
            r#"A fast tool

Usage: tool [OPTIONS] <COMMAND>

Commands:
  run      Run things
           with more lines
  check    Check things

Options:
  -v, --verbose...         Say more
      --color <WHEN>       When to color [possible values: auto, always, never]
      --out <DIR>          Output folder
  -h, --help               Print help
"#,
        );
        assert_eq!(spec.description.as_deref(), Some("A fast tool"));
        assert_eq!(
            spec.subcommands
                .iter()
                .map(|sub| (sub.name.all()[0].as_str(), sub.description.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("run", Some("Run things with more lines")),
                ("check", Some("Check things"))
            ]
        );
        let verbose = &spec.options[0];
        assert_eq!(verbose.name, names(&["-v", "--verbose"]));
        assert!(verbose.repeatable());
        assert_eq!(verbose.description.as_deref(), Some("Say more"));
        let color = &spec.options[1];
        assert_eq!(color.args.at(0).unwrap().name.as_deref(), Some("when"));
        assert_eq!(
            color.args.at(0).unwrap().suggestions,
            [
                Suggestion::Plain("auto".into()),
                Suggestion::Plain("always".into()),
                Suggestion::Plain("never".into())
            ]
        );
        let out = &spec.options[2];
        assert!(out.args.at(0).unwrap().template.folders());
        assert!(!out.args.at(0).unwrap().template.files());
    }

    #[test]
    fn gnu_help_is_read() {
        let spec = parse_help(
            "ls",
            r#"Usage: ls [OPTION]... [FILE]...
List information about the FILEs (the current directory by default).
Sort entries alphabetically if none of -cftuvSUX nor --sort is specified.

  -a, --all                  do not ignore entries starting with .
  -o FILE                    output to FILE
  -f, --file=FILE            read a file
"#,
        );
        assert_eq!(
            spec.description.as_deref(),
            Some("List information about the FILEs (the current directory by default)")
        );
        assert_eq!(spec.options.len(), 3);
        assert_eq!(spec.options[0].name, names(&["-a", "--all"]));
        let output = &spec.options[1];
        assert_eq!(output.args.at(0).unwrap().name.as_deref(), Some("file"));
        assert!(output.args.at(0).unwrap().template.files());
        assert_eq!(
            spec.options[2].args.at(0).unwrap().name.as_deref(),
            Some("file")
        );
        // the usage line gives the file arguments, so a command without a spec still
        // completes paths
        assert_eq!(spec.args.all().len(), 1);
        let files = spec.args.at(0).unwrap();
        assert!(files.template.files());
        assert!(files.is_optional && files.is_variadic);
    }

    #[test]
    fn usage_positionals_are_read() {
        let spec = parse_help(
            "cp",
            "Usage: cp [OPTION]... SOURCE... DEST\n  -r   Copy folders too\n",
        );
        assert_eq!(spec.args.all().len(), 2);
        assert!(spec.args.at(0).unwrap().template.files());
        assert!(spec.args.at(0).unwrap().is_variadic);
        assert!(spec.args.at(1).unwrap().template.files());
    }

    #[test]
    fn apt_and_zypper_command_rows() {
        let apt = parse_help(
            "apt-get",
            r#"Usage: apt-get [options] command

Commands:
  update - Retrieve new lists of packages
  install - Install new packages (pkg is libc6 etc...)
  reinstall - Reinstall packages
"#,
        );
        assert_eq!(
            apt.subcommands
                .iter()
                .map(|sub| (sub.name.all()[0].as_str(), sub.description.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("update", Some("Retrieve new lists of packages")),
                (
                    "install",
                    Some("Install new packages (pkg is libc6 etc...)")
                ),
                ("reinstall", Some("Reinstall packages"))
            ]
        );

        // zypper titles its command groups by topic, so its recipe reads every section
        let zypper = parse_help_with(
            "zypper",
            r#"Repository Management:

  repos, lr   List all defined repositories.
  addrepo     Add a new repository.
"#,
            &Hints {
                sections: Some(vec!["*".into()]),
            },
        );
        assert_eq!(zypper.subcommands[0].name, names(&["repos", "lr"]));
        assert_eq!(zypper.subcommands[1].name, Names::One("addrepo".into()));
    }

    #[test]
    fn two_options_on_one_line() {
        let spec = parse_help(
            "zip",
            r#"  -A              --adjust-sfx    Adjust self-extracting exe.
  -0              --store         Store without compression.
"#,
        );
        assert_eq!(
            spec.options
                .iter()
                .map(|option| option.name.all()[0].clone())
                .collect::<Vec<_>>(),
            ["-A", "--adjust-sfx", "-0", "--store"]
        );
        assert_eq!(
            spec.options[3].description.as_deref(),
            Some("Store without compression")
        );
    }

    #[test]
    fn yargs_options_and_commands() {
        let spec = parse_help(
            "serve",
            r#"Options:
  --port, -p   Port to listen on          [number] [default: 3000]
  --mode       Mode of operation          [string] [choices: "fast", "safe"]
Commands:
  serve <file>   Serve a file
"#,
        );
        let port = &spec.options[0];
        assert_eq!(port.name, names(&["--port", "-p"]));
        assert_eq!(port.args.at(0).unwrap().name.as_deref(), Some("port"));
        let mode = &spec.options[1];
        assert_eq!(
            mode.args.at(0).unwrap().suggestions,
            [
                Suggestion::Plain("fast".into()),
                Suggestion::Plain("safe".into())
            ]
        );
        assert_eq!(spec.subcommands[0].name, Names::One("serve".into()));
    }

    #[test]
    fn llama_cpp_help_is_read() {
        let spec = parse_help(
            "llama-cli",
            r#"----- common params -----
-m, --model FNAME          model path to load (env: LLAMA_ARG_MODEL)
-ngl, --n-gpu-layers N     number of layers to store in VRAM
--flash-attn [on|off|auto] flash attention (default: auto)
--cache-type-k TYPE        kv cache type (allowed values: f16, q8_0, q4_0)
"#,
        );
        assert_eq!(spec.options[0].name, names(&["-m", "--model"]));
        assert!(spec.options[0].args.at(0).unwrap().template.files());
        let flash = &spec.options[2];
        assert_eq!(
            flash.args.at(0).unwrap().suggestions,
            [
                Suggestion::Plain("on".into()),
                Suggestion::Plain("off".into()),
                Suggestion::Plain("auto".into())
            ]
        );
        assert_eq!(spec.options[3].args.at(0).unwrap().suggestions.len(), 3);
    }

    #[test]
    fn prose_sections_are_not_commands() {
        let spec = parse_help(
            "tool",
            r#"Usage: tool

Commands:
  go   Go somewhere

Author:
  Written by someone.

Exit status:
  0 if OK, 1 if not.
"#,
        );
        assert_eq!(spec.subcommands.len(), 1);
        assert_eq!(spec.subcommands[0].name, Names::One("go".into()));
    }

    #[test]
    fn annotations_do_not_reach_descriptions() {
        let spec = parse_help(
            "tool",
            r#"Options:
  -T, --tabsize COLS   Assume tab stops [env: TABSIZE=] [default: 8]
"#,
        );
        assert_eq!(
            spec.options[0].description.as_deref(),
            Some("Assume tab stops")
        );
        assert_eq!(
            spec.options[0].args.at(0).unwrap().name.as_deref(),
            Some("cols")
        );
    }

    #[test]
    fn descriptions_stop_at_one_sentence() {
        assert_eq!(
            sentence("do the thing. Then more"),
            Some("Do the thing".into())
        );
        assert_eq!(sentence("e.g. this stays"), Some("E.g. this stays".into()));
        assert_eq!(sentence("- nothing"), Some("Nothing".into()));
        assert_eq!(sentence("<file>"), None);
        let long = "a".repeat(100);
        assert_eq!(sentence(&long).unwrap().chars().count(), 100);
    }
}
