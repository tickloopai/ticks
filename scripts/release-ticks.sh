#!/bin/sh
# Builds, publishes and verifies the one Tick package selected by RELEASE_TAG=<package>-vX.Y.Z.
# Version commits and tags are created by scripts/release.mjs; the Release reuses the same tag.
# `build` makes the archive for one target, RELEASE_TARGET (default: this machine's), on a machine
# that can run it, and installs it with TICKLOOP (default: tickloop on PATH) as a user would.
# `publish` and `verify` need the archives of every target in $targets: a version is published for
# all of them or not at all.
set -eu

targets="aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl"
repository=tickloopai/ticks
release_downloads=https://github.com/$repository/releases/download
test_support_crate=tickloop-tick-test-support
download_attempts=6
download_retry_seconds=5
repository_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
dist_directory=$repository_root/dist/ticks
release_directory=$repository_root/dist/release
cd "$repository_root"

fail() {
    echo "$*" >&2
    exit 1
}

usage() {
    echo "usage: RELEASE_TAG=<package>-vX.Y.Z [RELEASE_TARGET=<target>] $0 build | [RELEASE_BRANCH=<branch>] $0 publish MARKETPLACE_CHECKOUT ARCHIVE_DIRECTORY | $0 verify ARCHIVE_DIRECTORY" >&2
    exit 2
}

manifest_field() {
    python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}

release_package() {
    python3 -I - "${RELEASE_TAG:-}" <<'PYTAG'
import json, re, sys
from pathlib import Path
match = re.fullmatch(r"([a-z][a-z0-9-]*)-v((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))", sys.argv[1])
if not match:
    sys.exit("RELEASE_TAG must be <package>-vX.Y.Z")
name, version = match.groups()
path = Path("packages") / name / "tick-package.json"
if not path.is_file():
    sys.exit(f"Unknown Tick package {name}")
manifest = json.loads(path.read_text())
if manifest["name"] != name or manifest["version"] != version:
    sys.exit(f"{path} does not match release tag {sys.argv[1]}")
print(name)
PYTAG
}

packages() {
    release_package
}

release_version() {
    package=$(release_package)
    manifest_field "packages/$package/tick-package.json" version
}

# archive_name PACKAGE VERSION TARGET
archive_name() {
    echo "tick-$1-$2-$3.tar.gz"
}

# The `file -b` description a target's program must match, as a grep -E pattern.
expected_program_type() {
    case $1 in
    aarch64-apple-darwin) echo '^Mach-O 64-bit executable arm64$' ;;
    x86_64-apple-darwin) echo '^Mach-O 64-bit executable x86_64$' ;;
    aarch64-unknown-linux-musl) echo '^ELF 64-bit LSB (pie )?executable, ARM aarch64, .*(statically|static-pie) linked' ;;
    x86_64-unknown-linux-musl) echo '^ELF 64-bit LSB (pie )?executable, x86-64, .*(statically|static-pie) linked' ;;
    *) fail "Unknown release target $1; expected one of: $targets" ;;
    esac
}

sha256_of() {
    shasum -a 256 "$1" | cut -d ' ' -f 1
}

summarize() {
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        printf '%s\n' "$*" >>"$GITHUB_STEP_SUMMARY"
    fi
}

build_release() {
    tickloop=${TICKLOOP:-tickloop}
    command -v "$tickloop" >/dev/null ||
        fail "No $tickloop to install the archives with; put tickloop on PATH or set TICKLOOP."
    target=${RELEASE_TARGET:-$(scripts/package-ticks.sh --host-target)}
    program_type=$(expected_program_type "$target")
    case $target in
    x86_64-apple-darwin) run_program="arch -x86_64" ;;
    *) run_program= ;;
    esac
    version=$(release_version)
    names=$(packages)
    for package in $names; do
        dependencies=$(cargo tree --locked --manifest-path "packages/$package/Cargo.toml" \
            --edges normal --target "$target" --prefix none)
        if printf '%s\n' "$dependencies" | grep -q "^$test_support_crate "; then
            fail "the $package program depends on $test_support_crate, which holds mock data"
        fi
    done

    # shellcheck disable=SC2086 # one word per package name
    TICK_TARGETS=$target TICK_PACKAGE_QUIET=1 scripts/package-ticks.sh $names

    scratch=$(mktemp -d "${TMPDIR:-/tmp}/release-ticks.XXXXXX")
    trap 'rm -rf "$scratch"' EXIT
    rm -rf "$release_directory"
    mkdir -p "$release_directory"
    for package in $names; do
        executable=$(manifest_field "packages/$package/tick-package.json" executable)
        name=$(archive_name "$package" "$version" "$target")
        archive=$dist_directory/$name

        unexpected=$(tar -tzf "$archive" |
            grep -Ev '^(tick-package\.json|LICENSE|THIRD_PARTY_NOTICES\.md|bin/.*|skills/.*|licenses/.*|vendor/([^/]+/)?|vendor/[^/]+/(LICENSE[^/]*|README\.md))$' ||
            true)
        [ -z "$unexpected" ] ||
            fail "$archive holds more than the package, its license notices and licenses: $unexpected"
        unpacked=$scratch/$package
        mkdir "$unpacked"
        # Python rather than tar: GNU tar cannot unpack into new directories under OrbStack's
        # x86_64 emulation, where local releases build the x86_64 Linux target.
        python3 -I -c '
import sys, tarfile
with tarfile.open(sys.argv[1]) as archive:
    if hasattr(tarfile, "data_filter"):
        archive.extractall(sys.argv[2], filter="data")
    else:
        archive.extractall(sys.argv[2])
' "$archive" "$unpacked"
        [ -d "$unpacked/skills" ] || fail "$archive has no skills/"
        [ -f "$unpacked/THIRD_PARTY_NOTICES.md" ] && [ -d "$unpacked/licenses" ] ||
            fail "$archive has no THIRD_PARTY_NOTICES.md and licenses/"
        program=$unpacked/$executable
        program_description=$(file -b "$program")
        printf '%s\n' "$program_description" | grep -Eq "$program_type" ||
            fail "$executable in $archive is not a program for $target: $program_description"
        case $program_description in
        *"dynamically linked"*) fail "$executable in $archive is not static: $program_description" ;;
        esac
        case $target in
        *-apple-darwin)
            codesign --verify "$program" || fail "$executable in $archive has no valid signature"
            codesign -dv "$program" 2>&1 | grep -qx 'Signature=adhoc' ||
                fail "$executable in $archive is not ad-hoc signed"
            ;;
        esac
        # shellcheck disable=SC2086 # run_program is empty or a command and its argument
        $run_program "$program" --help >/dev/null ||
            fail "$executable in $archive does not run on this machine"
        # shellcheck disable=SC2086
        [ "$($run_program "$program" --version)" = "$(basename "$executable") $version" ] ||
            fail "$executable in $archive does not report version $version"
        TICKLOOP_CONFIG_DIR=$scratch/config "$tickloop" tick install --path "$archive" >/dev/null

        cp "$archive" "$release_directory/$name"
        echo "Built and checked $package $version ($name, SHA-256 $(sha256_of "$archive"))."
    done
}

# After a failure that left releases behind: what exists, and the exact commands that finish or
# withdraw the release. Nothing is ever deleted here.
report_published() {
    stage=$1
    releases=$2
    [ -n "$releases" ] || return 0
    {
        echo
        echo "Releases in $repository:$releases. Nothing was deleted."
        echo "To finish, fix the cause and re-run the failed job (Re-run failed jobs in GitHub Actions); it"
        echo "keeps these releases. Locally, the same stage is:"
        echo "  RELEASE_TAG=$tag RELEASE_COMMIT=$commit GH_TOKEN=<token> $0 $stage"
        echo "To withdraw the release instead, revert its marketplace commit on the default branch if"
        echo "marketplace.json lists it, then run (the tag marks the version commit and stays):"
        for release in $releases; do
            echo "  gh release delete $release --repo $repository --yes"
        done
    } >&2
}

publish_release() {
    [ "$#" -eq 2 ] || usage
    checkout=$(CDPATH='' cd "$1" && pwd -P)
    archives=$(CDPATH='' cd "$2" && pwd -P)
    marketplace=$checkout/marketplace.json
    readme=$checkout/README.md
    [ -f "$marketplace" ] && [ -f "$readme" ] ||
        fail "$checkout is not a checkout of $repository: it has no marketplace.json or README.md"
    version=$(release_version)
    tag=$RELEASE_TAG
    commit=${RELEASE_COMMIT:-$(git rev-parse HEAD)}
    source=${GITHUB_REPOSITORY:-$repository}
    names=$(packages)
    for package in $names; do
        for target in $targets; do
            [ -f "$archives/$(archive_name "$package" "$version" "$target")" ] ||
                fail "$archives has no $target archive of $package $version; every target is built before any is published"
        done
    done
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/release-ticks.XXXXXX")
    published=""
    trap 'status=$?; rm -rf "$scratch"; if [ "$status" -ne 0 ]; then report_published "publish $checkout $archives" "$published"; fi' EXIT

    for package in $names; do
        release=$package-v$version
        files=""
        sums=""
        for target in $targets; do
            name=$(archive_name "$package" "$version" "$target")
            files="$files $archives/$name"
            sums="$sums
- $target: $(sha256_of "$archives/$name")"
        done
        if view=$(gh release view "$release" --repo "$repository" --json assets --jq '.assets[].name' 2>&1); then
            # An earlier attempt may have stopped part way: check what it uploaded, add the rest.
            for target in $targets; do
                name=$(archive_name "$package" "$version" "$target")
                if printf '%s\n' "$view" | grep -qxF "$name"; then
                    gh release download "$release" --repo "$repository" --pattern "$name" \
                        --dir "$scratch/$release" ||
                        fail "Could not download $name from the existing release $release."
                    [ "$(sha256_of "$scratch/$release/$name")" = "$(sha256_of "$archives/$name")" ] ||
                        fail "The release $release already exists with another $name; raise this package version to release again."
                else
                    gh release upload "$release" "$archives/$name" --repo "$repository" >/dev/null
                    echo "Added $name to the release $release."
                fi
            done
            published="$published $release"
            echo "The release $release carries every archive."
        elif printf '%s\n' "$view" | grep -qi 'not found'; then
            # shellcheck disable=SC2086 # one word per archive path
            gh release create "$release" $files --repo "$repository" --verify-tag \
                --title "$package $version" --notes "Tick package $package $version for macOS (Apple Silicon and Intel) and Linux (ARM64 and x86_64, static musl).

Install it with \`tickloop tick install $package\`.

Built from $source@$commit ($tag).
SHA-256:$sums" >/dev/null
            published="$published $release"
            echo "Created the release $release."
        else
            fail "Could not check the release $release: $(printf '%s\n' "$view" | head -n 1)"
        fi
        summarize "- $package $version: https://github.com/$repository/releases/tag/$release"
    done

    # shellcheck disable=SC2086 # one word per package name
    python3 -I - "$marketplace" "$readme" "$archives" "$targets" "$release_downloads" "$version" \
        $names <<'EOF'
import hashlib, json, sys

marketplace_path, readme_path, archives, targets, release_downloads, version, *names = sys.argv[1:]
targets = targets.split()

with open(marketplace_path) as marketplace_file:
    marketplace = json.load(marketplace_file)
packages = {package["name"]: package for package in marketplace["packages"]}
for name in names:
    with open(f"packages/{name}/tick-package.json") as manifest_file:
        description = json.load(manifest_file)["description"]
    downloads = {}
    for target in targets:
        archive_name = f"tick-{name}-{version}-{target}.tar.gz"
        with open(f"{archives}/{archive_name}", "rb") as archive_file:
            sha256 = hashlib.sha256(archive_file.read()).hexdigest()
        downloads[target] = {
            "url": f"{release_downloads}/{name}-v{version}/{archive_name}",
            "sha256": sha256,
        }
    listed = packages.get(name)
    if listed and tuple(map(int, listed["version"].split("."))) > tuple(map(int, version.split("."))):
        sys.exit(f"marketplace.json already lists newer {name} {listed['version']}; refusing to downgrade to {version}")
    if listed and listed["version"] == version:
        for target, download in listed["downloads"].items():
            if downloads.get(target, {}).get("sha256") != download.get("sha256"):
                sys.exit(f"marketplace.json lists {name} {version} with another {target} archive ({download.get('sha256')})")
    packages[name] = {
        "name": name,
        "version": version,
        "description": description,
        "downloads": downloads,
    }
marketplace["packages"] = sorted(packages.values(), key=lambda package: package["name"])
with open(marketplace_path, "w") as marketplace_file:
    json.dump(marketplace, marketplace_file, indent=2, ensure_ascii=False)
    marketplace_file.write("\n")

rows = []
for package in marketplace["packages"]:
    description = package["description"].replace("|", "\\|")
    rows.append(f"| [`{package['name']}`](packages/{package['name']}) | {package['version']} | {description} |\n")
section = (
    "## Packages\n\n"
    "Install one with `tickloop tick install <package>`.\n\n"
    "| Package | Version | Description |\n| --- | --- | --- |\n" + "".join(rows)
)
with open(readme_path) as readme_file:
    readme = readme_file.read()
start = readme.find("\n## Packages\n")
if start >= 0:
    end = readme.find("\n## ", start + 1)
    readme = readme[: start + 1] + section + (readme[end:] if end >= 0 else "")
else:
    readme = readme.rstrip("\n") + "\n\n" + section
with open(readme_path, "w") as readme_file:
    readme_file.write(readme)
EOF
    released=$(for package in $names; do printf '%s %s, ' "$package" "$version"; done)
    released=${released%, }
    if git -C "$checkout" diff --quiet -- marketplace.json README.md; then
        echo "The marketplace already lists $released."
    else
        # RELEASE_BRANCH lets a detached checkout of the default branch, as local releases use,
        # name the branch it updates.
        branch=${RELEASE_BRANCH:-$(git -C "$checkout" symbolic-ref --short HEAD)} ||
            fail "$checkout is not on a branch; set RELEASE_BRANCH"
        if [ -z "$(git -C "$checkout" config user.email || true)" ]; then
            export GIT_AUTHOR_NAME='github-actions[bot]'
            export GIT_AUTHOR_EMAIL='41898282+github-actions[bot]@users.noreply.github.com'
            export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME" GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
        fi
        git -C "$checkout" add marketplace.json README.md
        git -C "$checkout" commit --quiet \
            -m "feat: release $released" -m "Built from $source@$commit ($tag)."
        git -C "$checkout" push --quiet origin "HEAD:refs/heads/$branch"
        echo "Pushed the marketplace update ($(git -C "$checkout" rev-parse --short HEAD))."
    fi
    summarize "- marketplace.json lists $released."
}

verify_release() {
    [ "$#" -eq 1 ] || usage
    archives=$(CDPATH='' cd "$1" && pwd -P)
    version=$(release_version)
    tag=$RELEASE_TAG
    commit=${RELEASE_COMMIT:-$(git rev-parse HEAD)}
    published=$(for package in $(packages); do printf ' %s' "$package-v$version"; done)
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/release-ticks.XXXXXX")
    trap 'status=$?; rm -rf "$scratch"; if [ "$status" -ne 0 ]; then report_published "verify $archives" "$published"; fi' EXIT
    for package in $(packages); do
        for target in $targets; do
            name=$(archive_name "$package" "$version" "$target")
            expected=$(sha256_of "$archives/$name")
            url=$release_downloads/$package-v$version/$name
            attempt=1
            # Without credentials, as anyone installing the package downloads it.
            until curl -fsSL -o "$scratch/$name" "$url"; do
                [ "$attempt" -lt "$download_attempts" ] || fail "Could not download $url."
                attempt=$((attempt + 1))
                sleep "$download_retry_seconds"
            done
            downloaded=$(sha256_of "$scratch/$name")
            [ "$downloaded" = "$expected" ] || fail "$url has SHA-256 $downloaded, not $expected."
            echo "Verified $url."
        done
    done
    summarize "- Every download matches its SHA-256."
}

[ "$#" -ge 1 ] || usage
command=$1
shift
case $command in
build)
    [ "$#" -eq 0 ] || usage
    build_release
    ;;
publish) publish_release "$@" ;;
verify) verify_release "$@" ;;
*) usage ;;
esac
