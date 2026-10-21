#!/bin/sh
# Runs the package release stages that the GitHub release workflow runs, on this Mac and two
# OrbStack Linux machines instead, for `make release` (scripts/release.mjs --local), which runs
# build and publish but not the tests:
#
#   check SOURCE                     make check on this Mac and on both Linux machines, to run by
#                                    hand before releasing
#   build SOURCE ARCHIVES TAG...     scripts/release-ticks.sh build for every target of every tag,
#                                    each on a machine of its architecture, into ARCHIVES
#   publish ARCHIVES TAG...          scripts/release-ticks.sh publish and verify for every tag, from
#                                    this checkout at the release commit
#
# SOURCE must be under a directory the Linux machines share with this Mac, such as /Users. The
# machines are TICKS_LINUX_ARM64_MACHINE (default tl-arm) and TICKS_LINUX_X86_64_MACHINE (default
# tl-x86); a missing toolchain is installed in them. This Mac needs the aarch64-apple-darwin and
# x86_64-apple-darwin Rust targets and Rosetta, and gh logged in with write access to the repository.
set -eu

arm64_machine=${TICKS_LINUX_ARM64_MACHINE:-tl-arm}
x86_64_machine=${TICKS_LINUX_X86_64_MACHINE:-tl-x86}
mac_targets="aarch64-apple-darwin x86_64-apple-darwin"
tickloop_releases=https://github.com/tickloopai/homebrew-tap/releases/latest/download
repository_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cache_directory=$repository_root/target/release-local

fail() {
    echo "$*" >&2
    exit 1
}

usage() {
    echo "usage: $0 check SOURCE | build SOURCE ARCHIVES TAG... | publish ARCHIVES TAG..." >&2
    exit 2
}

machine_for() {
    case $1 in
    aarch64-unknown-linux-musl) echo "$arm64_machine" ;;
    x86_64-unknown-linux-musl) echo "$x86_64_machine" ;;
    *) fail "No Linux machine builds $1" ;;
    esac
}

# Runs a shell script in a Linux machine, in SOURCE, with its own Cargo target directory. This
# Mac's ~/.cargo/config.toml is visible there through /Users, so its rustc and C compiler wrappers
# are cleared.
in_machine() {
    machine=$1
    source=$2
    script=$3
    orb -m "$machine" env RUSTC_WRAPPER= HOST_CC=cc HOST_CXX=c++ CC_KNOWN_WRAPPER_CUSTOM= \
        RELEASE_SOURCE="$source" sh -c ". \"\$HOME/.cargo/env\" 2>/dev/null || true
export CARGO_TARGET_DIR=\$HOME/ticks-target
cd \"\$RELEASE_SOURCE\"
$script"
}

prepare_machine() {
    machine=$1
    command -v orb >/dev/null || fail "Local releases build Linux packages in OrbStack; install OrbStack or release with REMOTE=1."
    orb list | grep -q "^$machine " ||
        fail "No OrbStack machine $machine; create it (orb create ubuntu $arm64_machine; orb create --arch amd64 ubuntu $x86_64_machine) or set TICKS_LINUX_*_MACHINE."
    orb start "$machine" >/dev/null 2>&1 || true
    orb -m "$machine" sh -c '
set -e
missing=""
for program in cc musl-gcc file python3 node curl git; do
    command -v "$program" >/dev/null || missing=1
done
if [ -n "$missing" ]; then
    sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential musl-tools file python3 nodejs curl git ca-certificates >/dev/null
fi
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal -c rustfmt,clippy >/dev/null
fi
. "$HOME/.cargo/env"
rustup target add "$(uname -m)-unknown-linux-musl" >/dev/null 2>&1
if ! cargo nextest --version >/dev/null 2>&1; then
    case $(uname -m) in aarch64) platform=linux-arm ;; *) platform=linux ;; esac
    curl -LsSf "https://get.nexte.st/latest/$platform" | tar zxf - -C "$HOME/.cargo/bin"
fi
'
}

# Downloads the latest released Tickloop for a target into the cache, as the release workflow
# installs each archive with it, and prints its path.
mac_tickloop() {
    target=$1
    directory=$cache_directory/tickloop-$target
    archive=tickloop-$target.tar.gz
    mkdir -p "$directory"
    (
        cd "$directory"
        for name in "$archive" "$archive.sha256"; do
            curl -fsSL --retry 3 -o "$name" "$tickloop_releases/$name"
        done
        shasum -a 256 -c "$archive.sha256" >/dev/null
        tar -xzf "$archive" tickloop
    ) || fail "Could not download Tickloop for $target."
    echo "$directory/tickloop"
}

check() {
    [ "$#" -eq 1 ] || usage
    source=$(CDPATH='' cd "$1" && pwd -P)
    echo "Checking on this Mac…"
    (cd "$source" && make check) || fail "make check failed on this Mac; nothing was released."
    for machine in "$arm64_machine" "$x86_64_machine"; do
        echo "Checking on ${machine}…"
        prepare_machine "$machine"
        in_machine "$machine" "$source" 'make check' ||
            fail "make check failed on $machine; nothing was released."
    done
}

build() {
    [ "$#" -ge 3 ] || usage
    source=$(CDPATH='' cd "$1" && pwd -P)
    mkdir -p "$2"
    archives=$(CDPATH='' cd "$2" && pwd -P)
    shift 2
    mac_tickloop_path=$(mac_tickloop aarch64-apple-darwin)
    for machine in "$arm64_machine" "$x86_64_machine"; do
        prepare_machine "$machine"
        in_machine "$machine" "$source" "
mkdir -p \"\$HOME/.cache/ticks-release\" && cd \"\$HOME/.cache/ticks-release\"
archive=tickloop-\$(uname -m)-unknown-linux-musl.tar.gz
for name in \"\$archive\" \"\$archive.sha256\"; do
    curl -fsSL --retry 3 -o \"\$name\" \"$tickloop_releases/\$name\"
done
shasum -a 256 -c \"\$archive.sha256\" >/dev/null && tar -xzf \"\$archive\" tickloop" ||
            fail "Could not download Tickloop in $machine."
    done
    for tag in "$@"; do
        for target in $mac_targets; do
            (cd "$source" && RELEASE_TAG=$tag RELEASE_TARGET=$target TICKLOOP=$mac_tickloop_path \
                scripts/release-ticks.sh build) ||
                fail "Building $tag for $target failed; nothing was released."
            cp "$source"/dist/release/*-"$target".tar.gz "$archives/"
        done
        for target in aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
            machine=$(machine_for "$target")
            in_machine "$machine" "$source" "RELEASE_TAG=$tag RELEASE_TARGET=$target \
TICKLOOP=\$HOME/.cache/ticks-release/tickloop scripts/release-ticks.sh build" ||
                fail "Building $tag for $target on $machine failed; nothing was released."
            cp "$source"/dist/release/*-"$target".tar.gz "$archives/"
        done
    done
}

publish() {
    [ "$#" -ge 2 ] || usage
    archives=$(CDPATH='' cd "$1" && pwd -P)
    shift
    cd "$repository_root"
    branch=${RELEASE_BRANCH:-main}
    git fetch --quiet origin "$branch"
    checkout=$(mktemp -d "${TMPDIR:-/tmp}/ticks-marketplace.XXXXXX")
    git worktree add --quiet --detach "$checkout" FETCH_HEAD
    trap 'git worktree remove --force "$checkout" >/dev/null 2>&1 || true; rm -rf "$checkout"' EXIT
    for tag in "$@"; do
        commit=$(git rev-parse "$tag^{commit}")
        RELEASE_TAG=$tag RELEASE_COMMIT=$commit RELEASE_BRANCH=$branch \
            scripts/release-ticks.sh publish "$checkout" "$archives"
        RELEASE_TAG=$tag RELEASE_COMMIT=$commit scripts/release-ticks.sh verify "$archives"
    done
}

[ "$#" -ge 1 ] || usage
stage=$1
shift
case $stage in
check) check "$@" ;;
build) build "$@" ;;
publish) publish "$@" ;;
*) usage ;;
esac
