# Copr make_srpm entry point. Copr cds into the clone and runs
# `make -f .copr/Makefile srpm` with spec= and outdir=.
#
# make_srpm has no chroot package list in the Copr UI, so this
# target installs what it needs with dnf (cargo, rust, rpm-build,
# tar, xz, gzip). Do not require git: Copr already provides the
# tree as cwd. Override DNF (e.g. DNF=true) when the host already
# has those tools, such as GitHub Actions with apt + rustup.
#
# Source0 is this checkout. Source1 is a vendor tarball built here,
# not stored in git. cargo vendor's stdout (the source map,
# including git crates) is shipped in that tarball.
SHELL := /bin/bash
DNF ?= dnf

srpm:
	$(DNF) -y install cargo rust rpm-build tar xz gzip
	set -euo pipefail; \
	root=$$(pwd); \
	workdir=$$(mktemp -d); \
	payload=$$(mktemp -d); \
	cleanup() { rm -rf "$$workdir" "$$payload" "$$root/vendor"; }; \
	trap cleanup EXIT; \
	version=$$(awk '/^Version:/ { print $$2; exit }' "$(spec)"); \
	name=$$(awk '/^Name:/ { print $$2; exit }' "$(spec)"); \
	mkdir -p "$(outdir)"; \
	tar -C "$$root" -czf "$$workdir/$$name-$$version.tar.gz" \
		--exclude-vcs \
		--exclude target \
		--exclude vendor \
		--exclude .cargo \
		--transform "s,^\./,$$name-$$version/," \
		.; \
	( cd "$$root" && cargo vendor --locked --versioned-dirs vendor > "$$workdir/cargo-vendor-config.toml" ); \
	mv "$$root/vendor" "$$payload/vendor"; \
	cp "$$workdir/cargo-vendor-config.toml" "$$payload/cargo-vendor-config.toml"; \
	tar -C "$$payload" -cJf "$$workdir/$$name-$$version-vendor.tar.xz" \
		vendor cargo-vendor-config.toml; \
	rpmbuild -bs "$(spec)" \
		--define "_sourcedir $$workdir" \
		--define "_srcrpmdir $(outdir)" \
		--define "_disable_source_fetch 0"
