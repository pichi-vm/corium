# Copr "make srpm" entry point. Source0 is this checkout. Source1 is a
# vendor tarball built here, not stored in git. cargo vendor's stdout (the
# source map, including git crates) is shipped in that tarball.
SHELL := /bin/bash

srpm:
	set -euo pipefail; \
	root=$$(git rev-parse --show-toplevel); \
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
	if ! command -v cargo >/dev/null 2>&1; then \
		dnf -y install cargo; \
	fi; \
	( cd "$$root" && cargo vendor --locked --versioned-dirs vendor > "$$workdir/cargo-vendor-config.toml" ); \
	mv "$$root/vendor" "$$payload/vendor"; \
	cp "$$workdir/cargo-vendor-config.toml" "$$payload/cargo-vendor-config.toml"; \
	tar -C "$$payload" -cJf "$$workdir/$$name-$$version-vendor.tar.xz" \
		vendor cargo-vendor-config.toml; \
	rpmbuild -bs "$(spec)" \
		--define "_sourcedir $$workdir" \
		--define "_srcrpmdir $(outdir)" \
		--define "_disable_source_fetch 0"
