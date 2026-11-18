//! the line protocol of `compcore serve`, for frontends that can't link the crate, like a
//! shell script. records end with a nul byte, their fields are split by \x1f:
//!
//!   request  seq \x1f cursor \x1f cwd \x1f PATH \x1f buffer
//!   reply    seq \x1f start \x1f end ( \x1e label \x1f insert \x1f description \x1f kind )*
//!
//! cursor, start and end count chars. requests that arrive while one is answered replace
//! each other and only the newest is answered, so a frontend only ever waits for the reply
//! to its last request. that reply always comes: a request that can't be completed (a bad
//! field, a relative cwd) gets one without candidates. only a record without a seq gets none

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    sync::mpsc,
};

use crate::{Completions, Request, Specs, complete};

/// answer requests from `input` on `output` until `input` ends
pub fn run(
    input: impl BufRead + Send + 'static,
    mut output: impl Write,
    specs: &Specs,
    probe_help: bool,
) -> std::io::Result<()> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for record in input.split(0) {
            let Ok(record) = record else { break };
            if sender.send(record).is_err() {
                break;
            }
        }
    });
    let home = std::env::var_os("HOME").map(PathBuf::from);
    while let Ok(mut record) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            record = newer;
        }
        let Some(reply) = answer(
            &String::from_utf8_lossy(&record),
            home.clone(),
            specs,
            probe_help,
        ) else {
            continue;
        };
        output.write_all(reply.as_bytes())?;
        output.write_all(b"\0")?;
        output.flush()?;
    }
    Ok(())
}

/// the reply to one request, none when it has no seq to answer to
pub fn answer(
    record: &str,
    home: Option<PathBuf>,
    specs: &Specs,
    probe_help: bool,
) -> Option<String> {
    let mut fields = record.splitn(5, '\x1f');
    let seq: u64 = fields.next()?.parse().ok()?;
    let found = match request(fields, home, probe_help) {
        Some(request) => complete(&request, specs),
        None => Completions::default(),
    };
    let mut reply = format!("{seq}\x1f{}\x1f{}", found.start, found.end);
    for candidate in &found.candidates {
        let description = candidate.description.as_deref().unwrap_or_default();
        reply.push('\x1e');
        reply.push_str(&clean(&candidate.label));
        reply.push('\x1f');
        reply.push_str(&clean(&candidate.insert));
        reply.push('\x1f');
        reply.push_str(&clean(description));
        reply.push('\x1f');
        reply.push_str(candidate.kind.name());
    }
    Some(reply)
}

/// the request in the fields after the seq, none when one is missing or invalid
fn request<'a>(
    mut fields: impl Iterator<Item = &'a str>,
    home: Option<PathBuf>,
    probe_help: bool,
) -> Option<Request> {
    let cursor: usize = fields.next()?.parse().ok()?;
    let cwd = PathBuf::from(fields.next()?);
    let path_var = fields.next()?.to_string();
    let buffer = fields.next()?.to_string();
    // relative folders mean nothing to a process with its own cwd
    if !cwd.is_absolute() {
        return None;
    }
    Some(Request {
        buffer,
        cursor,
        cwd,
        path_var,
        home,
        probe_help,
    })
}

// control chars would break the record or the frontend's line
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Specs {
        let mut specs = Specs::load(None, None);
        specs.insert(
            "tool",
            r#"{ "name": "tool", "subcommands": [{ "name": "run", "description": "go\nnow" }] }"#,
        );
        specs
    }

    #[test]
    fn a_request_gets_its_candidates() {
        let reply = answer("7\x1f6\x1f/\x1f\x1ftool r", None, &specs(), false).unwrap();
        assert_eq!(reply, "7\x1f5\x1f6\x1erun\x1frun \x1fgo now\x1fsubcommand");
    }

    #[test]
    fn requests_that_cant_be_completed_get_an_empty_reply() {
        let empty = Some("1\x1f0\x1f0".to_string());
        assert_eq!(
            answer("1\x1f0\x1frel\x1f\x1f", None, &specs(), false),
            empty
        );
        assert_eq!(answer("1\x1f0\x1f/", None, &specs(), false), empty);
        assert_eq!(answer("1\x1fx\x1f/\x1f\x1f", None, &specs(), false), empty);
        // without a seq there is nothing to answer to
        assert!(answer("x\x1f0\x1f/\x1f\x1f", None, &specs(), false).is_none());
    }

    #[test]
    fn records_are_read_and_answered_until_the_input_ends() {
        let input = std::io::Cursor::new(b"1\x1f6\x1f/\x1f\x1ftool r\0".to_vec());
        let mut output = Vec::new();
        run(input, &mut output, &specs(), false).unwrap();
        assert!(output.starts_with(b"1\x1f5\x1f6\x1erun\x1f"));
        assert!(output.ends_with(b"\0"));
    }
}
