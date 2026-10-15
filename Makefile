# Phlegethon: build, install and package (Linux; Void is the reference platform).
#
#   make                 build the frontend and release binaries
#   make install         install to $(DESTDIR)$(PREFIX)  (default PREFIX=/usr/local)
#   make test            Rust (core, support crates), plugin and frontend tests
#   make dist            source tarball for xbps-src (checksum goes into the template)
#   make xbps            build the xbps package with xbps-src
#   make help

PREFIX  ?= /usr/local
DESTDIR ?=
BINDIR   = $(PREFIX)/bin
DATADIR  = $(PREFIX)/share
CARGO   ?= cargo
NPM     ?= npm

FRONTEND = packages/tauri-shell/frontend
TARGET   = target/release
ICON_SIZES = 32 128 256 512

.PHONY: all build frontend install uninstall test test-shell test-rust test-plugins test-frontend \
	desktop nmh-install dist xbps probe-check e2e-ui clean help

all: build

help:
	@sed -n '2,9p' $(MAKEFILE_LIST) | sed 's/^# \{0,1\}//'

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

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/phlegethon $(DESTDIR)$(BINDIR)/phlegethon-nmh
	rm -f $(DESTDIR)$(DATADIR)/applications/phlegethon.desktop
	rm -f $(DESTDIR)$(DATADIR)/metainfo/com.phlegethon.app.metainfo.xml
	for s in $(ICON_SIZES); do rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/$${s}x$${s}/apps/phlegethon.png; done
	rm -f $(DESTDIR)$(DATADIR)/doc/phlegethon/companion-setup.md
	-rmdir $(DESTDIR)$(DATADIR)/doc/phlegethon

test: test-rust test-plugins test-frontend

# Everything that does not need WebKitGTK headers.
test-rust:
	$(CARGO) test --workspace --exclude phlegethon-tauri

test-plugins:
	python3 packages/extractor-plugins/tests/test_parse_fixtures.py
	python3 packages/extractor-plugins/tests/test_extractors_offline.py

test-frontend:
	cd $(FRONTEND) && $(NPM) run typecheck && $(NPM) test

# The Tauri shell crate (needs webkit2gtk41-devel, gtk+3-devel, libsoup3-devel).
test-shell:
	$(CARGO) test -p phlegethon-tauri

desktop:
	@command -v desktop-file-validate >/dev/null && desktop-file-validate packaging/linux/phlegethon.desktop \
		|| echo "desktop-file-validate not installed (desktop-file-utils); skipped"
	@command -v appstreamcli >/dev/null && appstreamcli validate --no-net packaging/linux/com.phlegethon.app.metainfo.xml \
		|| echo "appstreamcli not installed (appstream); skipped"

# Usage: make nmh-install EXT=<extension id>
nmh-install:
	@test -n "$(EXT)" || { echo "usage: make nmh-install EXT=<32-letter extension id>"; exit 1; }
	scripts/install-nmh-linux.sh --extension-id $(EXT)

dist: frontend
	scripts/make-dist-tarball.sh

xbps:
	scripts/xbps-build.sh

# Probe the reference URLs with the real toolchain (needs network and yt-dlp).
probe-check:
	scripts/probe-check.sh

# Drive the real app window on every reference URL (needs a release build, tauri-driver,
# webkit2gtk-driver, python3-selenium, Xvfb or a display, ffprobe and yt-dlp).
e2e-ui:
	xvfb-run -a -s "-screen 0 1400x900x24" python3 packages/tauri-shell/e2e/reference_ui.py $(E2E_ARGS)

clean:
	$(CARGO) clean
	rm -rf $(FRONTEND)/dist dist
