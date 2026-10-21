.DEFAULT_GOAL := build

TICK_PACKAGES ?= $(notdir $(patsubst %/tick-package.json,%,$(wildcard packages/*/tick-package.json)))
TICKLOOP ?= tickloop
HOST_TARGET := $(shell scripts/package-ticks.sh --host-target)

.PHONY: build test check package-ticks dev-ticks release

build:
	cargo build --locked --workspace --bins

test:
	@cargo nextest --version >/dev/null 2>&1 || { echo 'make test requires cargo-nextest; install it with: cargo install --locked cargo-nextest' >&2; exit 1; }
	node --test scripts/*.test.mjs
	@mkdir -p target/test-config
	TICKLOOP_CONFIG_DIR=$$(mktemp -d "$(CURDIR)/target/test-config/config.XXXXXX") \
	NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
	cargo nextest run --locked --workspace --no-fail-fast

check:
	cargo fmt --all --check
	cargo clippy --locked --workspace --all-targets -- -D warnings
	$(MAKE) test

## Build the Tick packages for this machine (or TICK_TARGETS) into dist/ticks and print their marketplace.json entries.
package-ticks:
	scripts/package-ticks.sh $(TICK_PACKAGES)

## Package the Tick packages, then install each with TICKLOOP (default: tickloop on PATH).
dev-ticks:
	@command -v "$(TICKLOOP)" >/dev/null || { echo 'dev-ticks needs tickloop on PATH, or TICKLOOP=<path to tickloop>' >&2; exit 1; }
	@TICK_PACKAGE_QUIET=1 scripts/package-ticks.sh $(TICK_PACKAGES)
	@for package in $(TICK_PACKAGES); do \
		"$(TICKLOOP)" tick install --path "dist/ticks/$(HOST_TARGET)/$$package" || exit 1; \
	done

## Release each selected Tick package: check and build every target on this Mac and the OrbStack Linux
## machines, then commit the versions, push one <package>-vX.Y.Z tag each, publish and update the
## marketplace from here. REMOTE=1 pushes the tags and lets GitHub Actions check, build and publish.
## Select with TICK_PACKAGES="longbridge futu"; preview with: node scripts/release.mjs --check
release:
	@node scripts/release.mjs $(if $(filter 1,$(REMOTE)),--remote,--local)
