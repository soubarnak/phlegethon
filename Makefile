# Phlegethon: build, install and package (Linux; Void is the reference platform).
#
#   make                 build the frontend and release binaries
#   make install         install to $(DESTDIR)$(PREFIX)  (default PREFIX=/usr/local)
#   make test            Rust (core, support crates), plugin, script and frontend tests
#   make dist            source tarball for xbps-src (checksum goes into the template)
#   make xbps            build the xbps package with xbps-src (on Void)
#   make xbps-container  build the xbps package in a Void container (CONTAINER_ENGINE, LIBC)
#   make check-versions  check that every version location agrees
#   make help

PREFIX  ?= /usr/local
DESTDIR ?=
BINDIR   = $(PREFIX)/bin
DATADIR  = $(PREFIX)/share
CARGO   ?= cargo
NPM     ?= npm
# Container tool for `make xbps-container`. Podman is the default; rootless is fine.
# GitHub Actions sets CONTAINER_ENGINE=docker.
CONTAINER_ENGINE ?= podman
# glibc or musl: which Void image `make xbps-container` builds on.
LIBC ?= glibc

include packaging/void/images.env

FRONTEND = packages/tauri-shell/frontend
TARGET   = $(or $(CARGO_TARGET_DIR),target)/release
ICON_SIZES = 32 128 256 512

.PHONY: all build frontend install uninstall test test-shell gen-types test-rust test-plugins test-extension test-frontend \
	test-scripts desktop nmh-install dist xbps xbps-container check-versions probe-check e2e-ui clean help

all: build

help:
	@sed -n '2,10p' $(MAKEFILE_LIST) | sed 's/^# \{0,1\}//'

frontend:
	cd $(FRONTEND) && $(NPM) ci --no-audit --no-fund && $(NPM) run build

build:
	scripts/build-linux.sh

install:
	@test -x $(TARGET)/phlegethon -a -x $(TARGET)/phlegethon-nmh || { echo "run 'make' first"; exit 1; }
	install -Dm755 $(TARGET)/phlegethon $(DESTDIR)$(BINDIR)/phlegethon
	install -Dm755 $(TARGET)/phlegethon-nmh $(DESTDIR)$(BINDIR)/phlegethon-nmh
	install -Dm644 packaging/linux/phlegethon.desktop $(DESTDIR)$(DATADIR)/applications/phlegethon.desktop
	install -Dm644 packaging/linux/com.phlegethon.app.metainfo.xml $(DESTDIR)$(DATADIR)/metainfo/com.phlegethon.app.metainfo.xml
	for s in $(ICON_SIZES); do \
		install -Dm644 packaging/linux/icons/hicolor/$${s}x$${s}/apps/phlegethon.png \
			$(DESTDIR)$(DATADIR)/icons/hicolor/$${s}x$${s}/apps/phlegethon.png; \
	done
	install -Dm644 docs/companion-setup.md $(DESTDIR)$(DATADIR)/doc/phlegethon/companion-setup.md
	install -Dm644 LICENSE $(DESTDIR)$(DATADIR)/licenses/phlegethon/LICENSE

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/phlegethon $(DESTDIR)$(BINDIR)/phlegethon-nmh
	rm -f $(DESTDIR)$(DATADIR)/applications/phlegethon.desktop
	rm -f $(DESTDIR)$(DATADIR)/metainfo/com.phlegethon.app.metainfo.xml
	for s in $(ICON_SIZES); do rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/$${s}x$${s}/apps/phlegethon.png; done
	rm -f $(DESTDIR)$(DATADIR)/doc/phlegethon/companion-setup.md
	-rmdir $(DESTDIR)$(DATADIR)/doc/phlegethon
	rm -f $(DESTDIR)$(DATADIR)/licenses/phlegethon/LICENSE
	-rmdir $(DESTDIR)$(DATADIR)/licenses/phlegethon

test: test-rust test-plugins test-extension test-scripts test-frontend

# Everything that does not need WebKitGTK headers.
test-rust:
	$(CARGO) test --workspace --exclude phlegethon-tauri --locked

# Shell scripts (version check, native-messaging installer).
test-scripts:
	scripts/test-scripts.sh

test-extension:
	node --test packages/chrome-companion/session.test.js

test-plugins:
	python3 packages/extractor-plugins/tests/test_parse_fixtures.py
	python3 packages/extractor-plugins/tests/test_extractors_offline.py

test-frontend:
	cd $(FRONTEND) && $(NPM) run typecheck && $(NPM) test

# The Tauri shell crate (needs libwebkit2gtk41-devel, gtk+3-devel, libsoup3-devel).
test-shell:
	$(CARGO) test -p phlegethon-tauri --locked

# Regenerate the TypeScript IPC types from the Rust serde types (ts-rs, `ts` feature). Needs the shell libraries too.
gen-types:
	scripts/gen-ts-types.sh

# Validate the desktop entry and the AppStream metadata. A missing validator is skipped;
# a validation failure fails the target.
desktop:
	@if command -v desktop-file-validate >/dev/null; then \
		desktop-file-validate packaging/linux/phlegethon.desktop; \
	else \
		echo "desktop-file-validate not installed (desktop-file-utils); skipped"; \
	fi
	@if command -v appstreamcli >/dev/null; then \
		appstreamcli validate --no-net packaging/linux/com.phlegethon.app.metainfo.xml; \
	else \
		echo "appstreamcli not installed (appstream); skipped"; \
	fi

# Usage: make nmh-install EXT=<extension id>
nmh-install:
	@test -n "$(EXT)" || { echo "usage: make nmh-install EXT=<32-letter extension id>"; exit 1; }
	scripts/install-nmh-linux.sh --extension-id $(EXT)

dist: frontend
	scripts/make-dist-tarball.sh

xbps:
	scripts/xbps-build.sh

# Build the xbps package in a pinned Void container (packaging/void/images.env). Run `make dist` first.
# The container needs --privileged for the uchroot build chroot (Docker). Rootless podman builds
# with the ethereal chroot style, chosen by build-in-container.sh (see README).
xbps-container:
	@test -f dist/phlegethon-$$(sed -n 's/^version=//p' packaging/void/srcpkgs/phlegethon/template).tar.gz \
		|| { echo "run 'make dist' first"; exit 1; }
	$(CONTAINER_ENGINE) run --rm --privileged -v "$$PWD:/src" \
		$(if $(filter musl,$(LIBC)),$(VOID_IMAGE_MUSL),$(VOID_IMAGE_GLIBC)) \
		/src/packaging/void/build-in-container.sh

check-versions:
	scripts/check-versions.sh

# Probe the reference URLs with the real toolchain (needs network and yt-dlp).
probe-check:
	scripts/probe-check.sh

# Drive the real app window on every reference URL (needs a release build, tauri-driver,
# webkit2gtk-driver, python3-selenium, Xvfb or a display, ffprobe and yt-dlp). The WebKit
# sandbox stays on; PHLEGETHON_E2E_NO_SANDBOX=1 turns it off inside containers.
e2e-ui:
	xvfb-run -a -s "-screen 0 1400x900x24" python3 packages/tauri-shell/e2e/reference_ui.py $(E2E_ARGS)

clean:
	$(CARGO) clean
	rm -rf $(FRONTEND)/dist dist
