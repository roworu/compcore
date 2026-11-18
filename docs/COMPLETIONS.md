# completions

how the bundled specs are made, and how the engine turns a command line into candidates.
frontends only draw what it returns, see the README.

## part 1: how specs are made

there is one parser (`crates/compcore-spec`) and one spec format. the same parser runs at
build time to make the bundled cache and at runtime to read a command's help when no spec
knows it yet. adding a command is data: a line in `tools/generator/manifest.json`, plus an
overlay when its help is unusual.

### rules

- **never run the programs on the host.** the generator runs every program it describes, so
  it only ever runs inside a throwaway podman container: `tools/generator/generate.sh`
  starts one, installs the programs, writes the json and removes the container. the container
  only sees `tools/generator/` (read only) and `specs/` (where specs are
  written).
- **specs are data, never code.** fig's `generators`, `script`, `custom` and function valued
  fields are ignored by the engine, so don't add them; they would only bloat the file. anything
  that would need running a command to know (git branches, docker containers) stays without
  suggestions. package names are the one exception: the engine lists the package manager's
  local cache itself, see part 2.
- **build specs from the real program**, its `--help` (or `help <cmd>`) in the container, not
  from memory. the parser is generic; an overlay is for what no help text can say: argument
  kinds, value suggestions, priorities, and programs whose help isn't in a parseable shape.
- one file per command: `specs/<command>.json`, `"name"` equal to the file name.
  the aws spec is the exception, it is shipped gzipped as `aws.json.gz`.

### the spec format

fig's spec format as json ([fig docs](https://fig.io/docs/reference/subcommand)), the subset
the engine reads:

```json
{
  "name": "tool",
  "description": "One line, capitalized, no final period",
  "subcommands": [
    { "name": ["remove", "rm"], "description": "Remove things", "priority": 76,
      "args": { "name": "file", "template": "filepaths", "isVariadic": true },
      "options": [ ... ], "subcommands": [ ... ] }
  ],
  "options": [
    { "name": ["-v", "--verbose"], "description": "Say more", "isPersistent": true, "isRepeatable": true },
    { "name": "--color", "args": { "name": "when", "isOptional": true, "suggestions": ["always", "never"] } },
    { "name": "--out", "args": { "name": "dir", "template": "folders" } }
  ],
  "args": [{ "name": "first", "suggestions": [{ "name": "a", "description": "The a value" }] }, { "name": "rest", "isVariadic": true }]
}
```

- `priority` 0 to 100, 50 when left out: give the few subcommands people type most 75 or 76,
  they are listed first. a manifest entry's `top` list does it.
- an option whose argument isn't optional takes the next word, so set `args` only when it
  really takes one, and `"isOptional": true` for `--opt[=VALUE]` forms.
- `template`: `"filepaths"` (files and folders) or `"folders"`; args without one complete
  nothing, except their `suggestions`.
- `isPersistent` options are also taken after subcommands, use it for global options.
- `hidden` hides a name until it is typed whole.

### the parser

`crates/compcore-spec/src/help.rs` turns help text into the spec above. it is one pipeline,
not one parser per program:

- ansi and osc escapes are stripped, then rows are split at their name and description
  columns (two or more spaces or a tab), with wrapped description lines joined by indentation.
- options: `-a, --all`, `--file=FILE`, `--color[=WHEN]`, `-o <file>`, `-M --machine=NAME`,
  `-f FILE`, repeatable `...`; two options on one line (zip) are split.
- subcommands: `name   description`, `name, alias   description`, apt's `name - description`,
  yargs' program-prefixed names, under section titles mentioning commands (a recipe can widen
  or narrow this with `sections`).
- values: `<a|b>`, `{a,b}`, `[a|b]`, `[possible values: ...]`, `allowed values: ...`, yargs
  `[choices: ...]`; annotations like `[env: ...]` and `[default: ...]` are dropped.
- argument names imply templates (`FILE`, `path`, `source`, `dir`, `folder`, ...), and a
  usage line's positionals become the command's `args` so even an unknown command completes
  files.

### generating specs

`tools/generator/manifest.json` groups commands by the container image their programs come
from (`ubuntu`, `fedora`, `arch`, `opensuse`, `alpine`, `node` for tools installed from npm,
and `llama` for the llama.cpp tools). each group has the image, an install command, and its
commands. a command is either a name or an object:

```json
{
  "name": "zypper",
  "description": "openSUSE package manager",
  "help": ["help"],
  "subHelp": ["help", "{}"],
  "sections": ["*"],
  "depth": 1,
  "top": ["install", "remove", "update"],
  "persistent": true,
  "subArgs": { "install": { "name": "package", "isVariadic": true } },
  "overlay": "zypper.json"
}
```

- `help`: arguments that print the help, default `["--help"]`.
- `subHelp`: template for a subcommand's help, `{}` stands for its path; `depth` is how many
  levels down to ask (default 2), 0 keeps only the top help.
- `sections`: words a command list's section title contains; none means titles mentioning
  commands. `["*"]` reads every section except options, examples, files and prose.
- `top`: subcommands to list first, in order.
- `persistent`: the top options also work after subcommands.
- `args`: what the program takes, when the usage line isn't enough.
- `subArgs`: arguments by subcommand path, like `install` or `pip install`.
- `overlay`: a json file in `tools/generator/overlays/` merged over the generated spec.
  objects merge, arrays of named items merge by name (a `null` entry drops one), other arrays
  replace. overlays hold priorities, suggestions and argument kinds the help can't express.
- `extraHelp`: more help invocations merged into the top spec, like pacman's per-operation
  help.
- `handwritten`: the spec is written by hand and regeneration skips it.

#### add a command

1. find the image that ships it. ubuntu is the default, use another distro's image only for
   that distro's own tools (dnf and rpm from fedora, pacman from arch, zypper from opensuse,
   apk from alpine).
2. add the package to that group's `install` line and the command to its `commands`, for
   example

   ```json
   { "name": "jq", "description": "Command-line JSON processor", "args": { "name": "filter" } }
   ```

   look at the raw help first, inside the container:

   ```sh
   podman run --rm -it docker.io/library/ubuntu:26.04 sh -c 'apt-get update && apt-get install -y jq && jq --help'
   ```

3. generate only that command: `sh tools/generator/generate.sh ubuntu jq`. the first run
   builds the static generator in the dev image.
4. read the json it wrote. check that subcommands aren't missing (a wrong `sections` is the
   usual cause), descriptions aren't cut mid-word nonsense, options that take a value have
   `args`, and files or folders have a template. fix it in the manifest or an overlay, not in
   the json, so the next regeneration keeps the fix.
5. add the file to `BUNDLED` in `crates/compcore/src/spec.rs`, sorted. the test
   `every_bundled_spec_is_listed_and_parses` fails until every file is listed and parses.
6. run the tests (`sh scripts/build-test.sh`, see AGENTS.md) and look at the candidates by
   hand: `compcore complete 'jq -'`.

programs whose help is a usage line or a man page only (git, ssh, scp, find, less, ip, man,
which, cd, docker, cargo, npm) are written by hand: write the json directly, from the
program's man page or `--help` read in the container, and mark the command `handwritten` in
the manifest. runtime probing covers any other program the user has, see part 2.

#### refresh after an upgrade

`sh tools/generator/generate.sh <image>` regenerates a whole group. review `git diff
specs`: removed options are fine when the program dropped them, a spec that
shrank a lot means the help format changed and the parser or an overlay needs a fix.

### aws

aws prints no parseable help, it has command tables instead: `sh tools/generator/generate.sh
aws` installs the aws cli in a container and walks them (`tools/generator/aws.py`): every
service, its operations with their first help sentence, their options, and fixed values of
up to 20 choices. option descriptions are only kept for the global options, they would
triple the size. the json is gzipped to `specs/aws.json.gz`, which is a few
hundred kilobytes instead of megabytes; the engine decompresses it when aws is first completed.


## part 2: the engine

### design

inshellisense (MIT, typescript on node) wraps the shell in its own pty and reads the command
line from the screen. the frontends get the exact buffer and cursor from zle, so its shell
layer is not used. its engine (split the line into words, walk the spec through
subcommands, options and their arguments, then filter by prefix) is ported to rust. bundling
node for it would add a runtime for little gain.

fig's specs (MIT) are typescript modules. the engine reads their json form, the subset in part 1;
`generators`, `script` and other code are ignored.

about 135 commands are bundled: core tools (ls, cp, grep, find, tar, curl, ssh, rsync, ps,
systemctl, journalctl, ...), package managers (apt, apt-get, apt-cache, dpkg, dnf, yum, rpm,
pacman, zypper, apk, brew, snap, flatpak, pip), git, cargo, docker, npm, podman, python, aws with all its services
and operations, and the llama.cpp tools. each spec is embedded in the binary but only parsed
when its command is completed. a `<command>.json` in the specs folder the frontend passes
(`Specs::load(Some(dir), ..)`, `compcore --specs DIR`) replaces the bundled one.

an argument with a `packages:<manager>` template (apt, dnf or yum, pacman, zypper, apk, brew,
flatpak) completes the names in that manager's local cache, read once per process.

for a command with no spec, the engine (unless the frontend turns probing off) runs
`<command> [sub] --help` once, parses it with the same parser, and caches the result in memory
and in the cache folder the frontend gives `Specs::load` (keyed by the program's path, size and mtime, so a
new version is read again). the top help and its immediate subcommands are read, at most 24
of them, with a one second deadline each. probing only happens for bare command names found
on the shell's PATH: a command typed as `./tool` or `/opt/tool` never runs just because a word
was typed. bundled and user specs never probe.

each candidate has a kind: subcommand, suggestion, option, command, folder or file. that is
also the order of candidates that match equally well; frontends color them by kind.


### safety

bundled and user specs are declarative and never run anything. package names come from the
manager's local cache with cache-only queries (`apt-cache pkgnames`, `dnf -C repoquery
--available`, `pacman -Ssq`, `zypper --no-refresh search`, `apk search`, `brew formulae`,
`flatpak remote-ls --cached`): they print names, don't touch the network and change nothing.
commands come from listing PATH, files from listing folders, the rest from the specs.

runtime probing is the one place the engine starts a program to find completions: the command the
user is already typing, with stdin closed, a fixed `C` locale, pagers disabled, a one second
deadline and a 256 KiB output cap. it is on by default because it is the only way a random
cli gets useful completions; `Request::probe_help` false and `compcore --no-probe` turn it
off. it is still an implicit
execution path for a program on PATH, so providers that need more (running `git branch`,
`docker ps`) remain unsupported.

### frontends

the engine knows nothing about the ui. a rust frontend links the crate and calls
`compcore::complete(&Request, &Specs)`, best off its ui thread. anything else runs
`compcore serve` once per session and talks to it over stdin and stdout (`crates/compcore/src/serve.rs`).
records end with a nul byte and their fields are split by `\x1f`:

```
request  seq \x1f cursor \x1f cwd \x1f PATH \x1f buffer
reply    seq \x1f start \x1f end ( \x1e label \x1f insert \x1f description \x1f kind )*
```

cursor, start and end count chars; the frontend replaces chars start..end with the picked
insert, which is already quoted for the shell. requests queued while one is answered replace
each other and only the newest gets a reply, so a frontend never needs a timeout: it waits
for the reply to its last request, which always comes (without candidates when the request
can't be completed), and drops replies to older seqs.
control chars in replies become spaces. compcore exits when its stdin ends.
