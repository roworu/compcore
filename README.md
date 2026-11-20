# compcore

shell completion engine: given a command line and a cursor, the candidates for the word at the
cursor, from fig-style json specs of ~135 commands, programs on PATH, the filesystem, package
manager caches and, for commands without a spec, their own `--help`.

it knows nothing about how candidates are shown, so any frontend can draw them: a rust
program links the `compcore` crate, anything else (a shell script, an editor plugin, another
language) runs `compcore serve` and talks to it over stdin and stdout.

## build

```sh
# fmt, clippy and tests, in podman
sh scripts/build-test.sh
# a static binary for frontends that run it
podman run --rm -v "$PWD":/src:Z compcore-dev \
  cargo build --release --target x86_64-unknown-linux-musl -p compcore
install -Dm755 target/podman/x86_64-unknown-linux-musl/release/compcore ~/.local/bin/compcore
```

## use

```sh
compcore complete 'git ch'          # candidates of a line, one per line
compcore serve [--specs DIR]        # answer frontends on stdin/stdout
               [--no-probe]         # never run `<command> --help`
compcore --version                  # or --help
```

or could be used bundled, for example: https://github.com/roworu/compui

the protocol and how specs are made and refreshed are in `docs/COMPLETIONS.md`.

