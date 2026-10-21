#!/bin/sh
# Builds each named Tick package under packages/ for each target in TICK_TARGETS (default: this
# machine's, see --host-target), assembles its package directory in dist/ticks/<target>/<package>/
# and its release archive in dist/ticks/, and prints the archive's SHA-256 and the matching
# marketplace.json entry. Every package carries this repository's LICENSE and
# THIRD_PARTY_NOTICES.md with the license copies it names, under the paths it names them by. It
# publishes nothing. With TICK_PACKAGE_QUIET set it only names each program it builds, for
# make dev-ticks and the release build (scripts/release-ticks.sh build).
#
# Linux targets are musl, matching the Linux Tickloop, and must be built on a machine of that
# architecture with musl-tools installed. Programs for macOS are ad-hoc signed, since the linker
# signs only arm64 ones by itself.
set -eu

release_downloads=https://github.com/tickloopai/ticks/releases/download
# The license, the third-party notices and the license copies they name, which every package carries.
license_notices="LICENSE THIRD_PARTY_NOTICES.md licenses vendor/longbridge-oauth/LICENSE-MIT
vendor/longbridge-oauth/LICENSE-APACHE vendor/longbridge-oauth/README.md"
repository_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
target_directory=${CARGO_TARGET_DIR:-$repository_root/target}
dist_directory=$repository_root/dist/ticks
cd "$repository_root"

host_target() {
    case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) echo aarch64-apple-darwin ;;
    Darwin-x86_64) echo x86_64-apple-darwin ;;
    Linux-aarch64) echo aarch64-unknown-linux-musl ;;
    Linux-x86_64) echo x86_64-unknown-linux-musl ;;
    *)
        echo "No Tick package target for $(uname -s) $(uname -m)" >&2
        exit 1
        ;;
    esac
}

if [ "${1:-}" = --host-target ]; then
    host_target
    exit 0
fi
if [ "$#" -eq 0 ]; then
    echo "usage: $0 PACKAGE... | $0 --host-target" >&2
    exit 2
fi
targets=${TICK_TARGETS:-$(host_target)}

manifest_field() {
    python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}

# No AppleDouble files, extended attributes or owner names from this machine.
archive_package() {
    archive=$1
    shift
    if tar --version 2>/dev/null | grep -q 'GNU tar'; then
        tar --no-xattrs --no-acls --owner=0 --group=0 --numeric-owner -czf "$archive" "$@"
    else
        COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --no-acls \
            --uid 0 --gid 0 --numeric-owner -czf "$archive" "$@"
    fi
}

mkdir -p "$dist_directory"
for package in "$@"; do
    source_directory=packages/$package
    manifest=$source_directory/tick-package.json
    name=$(manifest_field "$manifest" name)
    version=$(manifest_field "$manifest" version)
    executable=$(manifest_field "$manifest" executable)
    if [ "$name" != "$package" ]; then
        echo "$manifest names the package $name, not $package" >&2
        exit 1
    fi
    program=$(basename "$executable")
    for target in $targets; do
        if [ -n "${TICK_PACKAGE_QUIET:-}" ]; then
            echo "Building ${program} for ${target}…"
            cargo_output=--quiet
        else
            cargo_output=
        fi
        cargo build --locked --release $cargo_output --target "$target" \
            --manifest-path "$source_directory/Cargo.toml" --bin "$program"

        package_directory=$dist_directory/$target/$package
        rm -rf "$package_directory"
        mkdir -p "$package_directory/$(dirname "$executable")"
        cp "$manifest" "$package_directory/tick-package.json"
        cp "$target_directory/$target/release/$program" "$package_directory/$executable"
        case $target in
        *-apple-darwin)
            codesign --verify "$package_directory/$executable" 2>/dev/null ||
                codesign --sign - "$package_directory/$executable"
            ;;
        esac
        cp -R "$source_directory/skills" "$package_directory/skills"
        for notice in $license_notices; do
            mkdir -p "$package_directory/$(dirname "$notice")"
            cp -R "$notice" "$package_directory/$notice"
        done

        archive_name=tick-$package-$version-$target.tar.gz
        archive=$dist_directory/$archive_name
        archive_package "$archive" -C "$package_directory" tick-package.json "${executable%%/*}" \
            skills LICENSE THIRD_PARTY_NOTICES.md licenses vendor
        if [ -n "${TICK_PACKAGE_QUIET:-}" ]; then
            continue
        fi
        sha256=$(shasum -a 256 "$archive" | cut -d ' ' -f 1)
        printf '\nPackage directory: %s\nArchive: %s\nSHA-256: %s\nmarketplace.json entry:\n' \
            "$package_directory" "$archive" "$sha256"
        python3 -I -c '
import json, sys
manifest_path, target, url, sha256 = sys.argv[1:]
with open(manifest_path) as manifest_file:
    manifest = json.load(manifest_file)
entry = {
    "name": manifest["name"],
    "version": manifest["version"],
    "description": manifest["description"],
    "downloads": {target: {"url": url, "sha256": sha256}},
}
print(json.dumps(entry, indent=2, ensure_ascii=False))
' "$manifest" "$target" "$release_downloads/$package-v$version/$archive_name" "$sha256"
    done
done
