#!/bin/sh
# regenerate the bundled completion specs from the programs themselves, inside a throwaway
# container, so nothing a recipe runs touches the host. writes into specs/.
#   sh tools/generator/generate.sh <ubuntu|fedora|arch|opensuse|alpine|node|llama> [command...]
#   sh tools/generator/generate.sh aws
# the commands, their packages and their help quirks are data in manifest.json; the parser is
# the same one the engine runs at runtime (crates/compcore-spec).
set -e
dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$dir/../.." && pwd)
out=$(cd "$root/specs" && pwd)
key=${1:?usage: generate.sh <group|aws> [command...]}
shift

if [ "$key" = aws ]; then
	podman run --rm -v "$dir":/gen:ro,Z -v "$out":/out:Z docker.io/library/ubuntu:26.04 sh -c '
		apt-get update -qq >/dev/null && apt-get install -y -qq python3-venv gzip >/dev/null 2>&1 &&
		python3 -m venv /venv && /venv/bin/pip install -q awscli &&
		/venv/bin/aws --version && /venv/bin/python /gen/aws.py | gzip -9 > /out/aws.json.gz'
	exit
fi

# the generator runs in every distro image, so build it static once. this also keeps it
# independent of the host
case $(uname -m) in
x86_64) target=x86_64-unknown-linux-musl ;;
aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
*)
	echo "unsupported host architecture $(uname -m)" >&2
	exit 1
	;;
esac
tool="$root/target/podman/$target/release/compcore-gen"

if [ ! -x "$tool" ]; then
	podman build -q -t compcore-dev - <"$root/Containerfile"
	podman run --rm -v "$root":/src:Z \
		-v compcore-cargo-registry:/usr/local/cargo/registry \
		-v compcore-cargo-git:/usr/local/cargo/git \
		compcore-dev sh -c "cargo build --release --target $target -p compcore-gen"
fi

line=$(podman run --rm -v "$dir":/gen:ro,Z -v "$tool":/tool:ro,Z compcore-dev \
	/tool --images --manifest /gen/manifest.json | awk -F '\t' -v key="$key" '$1 == key')
if [ -z "$line" ]; then
	echo "unknown group $key" >&2
	exit 1
fi
image=$(printf '%s\n' "$line" | cut -f2)
install=$(printf '%s\n' "$line" | cut -f3)

podman run --rm \
	-v "$dir":/gen:ro,Z -v "$out":/out:Z -v "$tool":/tool:ro,Z "$image" \
	sh -c "{ $install; } >/dev/null 2>&1 && /tool --group $key --manifest /gen/manifest.json --out /out --overlays /gen/overlays $*"
