# RawVibes — build, run and package.
#   make run FILE=photo.RW2     debug run, optionally opening a photo
#   make release / run-release  optimised build
#   make install PREFIX=~/.local
#   make arch                   build an Arch Linux package with makepkg
#   make dmg                    build a macOS .dmg (run on macOS, needs dylibbundler)
#   make pot                    refresh po/rawvibes.pot and merge it into po/*.po
#   make models                 fetch and convert the AI denoise weights (needs curl, unzip, python3 + onnx, numpy)

PREFIX  ?= /usr/local
DESTDIR ?=
CARGO   ?= cargo
FILE    ?=

APP_ID  := io.github.rawvibes.RawVibes
BIN     := rawvibes
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCHDIR := packaging/arch
LINGUAS := $(basename $(notdir $(wildcard po/*.po)))
MOS     := $(foreach l,$(LINGUAS),target/locale/$(l)/LC_MESSAGES/$(BIN).mo)
# RawNIND UtNet2 as darktable ships it (GPL-3.0 + CC-BY-4.0 weights, see models/README.md).
MODELS_URL := https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/rawdenoise-nind.dtmodel
MODELS_SHA := d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af
MODELS     := $(foreach v,bayer linear,models/rawnind/$(v).bin models/rawnind/$(v).json)
DTMODEL    := target/models/rawdenoise-nind.dtmodel
# macOS: shasum (its sha256sum -c reads no stdin); Linux: sha256sum.
SHA256     := $(shell command -v shasum >/dev/null && echo shasum -a 256 || echo sha256sum)

.PHONY: all build run release run-release test check fmt clean install uninstall arch dmg pot locale models lensfun

all: release

build: locale lensfun
	$(CARGO) build

run: locale lensfun
	$(CARGO) run -p $(BIN) -- $(FILE)

release: locale lensfun
	$(CARGO) build --release

run-release: locale lensfun
	$(CARGO) run --release -p $(BIN) -- $(FILE)

test:
	$(CARGO) test --workspace

check:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all

locale: $(MOS)

# The lensfun database (CC-BY-SA 3.0) as JSON for the Lens section; none found, no profiles.
LENSFUN_DB ?= $(firstword $(wildcard /usr/share/lensfun/version_1 /opt/homebrew/share/lensfun/version_1 /usr/local/share/lensfun/version_1))
lensfun: target/lensfun.json

target/lensfun.json: tools/lensfun_db.py
	@if [ -n "$(LENSFUN_DB)" ]; then python3 -I tools/lensfun_db.py $(LENSFUN_DB) $@; \
	  else echo "lensfun database not found: no lens profiles"; fi

target/locale/%/LC_MESSAGES/$(BIN).mo: po/%.po
	@mkdir -p $(dir $@)
	msgfmt --check -o $@ $<

pot:
	xgettext -L Rust --from-code=UTF-8 -k --keyword=gettext --keyword='gettext!' \
	  --package-name=RawVibes -o po/$(BIN).pot gtk/src/*.rs
	for l in $(LINGUAS); do msgmerge -U --backup=none po/$$l.po po/$(BIN).pot; done

target/release/$(BIN):
	$(CARGO) build --release

models: $(MODELS)

# Downloaded unless already there (the PKGBUILD puts makepkg's copy there).
$(DTMODEL):
	mkdir -p target/models
	curl -LsS --fail -o $@.part $(MODELS_URL)
	mv $@.part $@

# One run writes every file; a failed one leaves none behind to look done.
$(MODELS) &: $(DTMODEL)
	test "$$($(SHA256) $(DTMODEL) | cut -d' ' -f1)" = $(MODELS_SHA) || { echo "$(DTMODEL): wrong checksum"; exit 1; }
	unzip -o -q $(DTMODEL) -d target/models
	python3 tools/convert_rawnind.py target/models/rawdenoise-nind models/rawnind || { rm -rf models/rawnind; exit 1; }

# Installs the AI models when `make models` fetched them (no network here: packagers run it first).
install: target/release/$(BIN) locale lensfun
	install -Dm755 target/release/$(BIN) $(DESTDIR)$(PREFIX)/bin/$(BIN)
	for l in $(LINGUAS); do install -Dm644 target/locale/$$l/LC_MESSAGES/$(BIN).mo \
	  $(DESTDIR)$(PREFIX)/share/locale/$$l/LC_MESSAGES/$(BIN).mo; done
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 -t $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/actions data/icons/hicolor/scalable/actions/*.svg
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/$(BIN)/LICENSE
	if [ -f target/lensfun.json ]; then install -Dm644 target/lensfun.json $(DESTDIR)$(PREFIX)/share/$(BIN)/lensfun.json; \
	  install -Dm644 LENSFUN.md $(DESTDIR)$(PREFIX)/share/licenses/$(BIN)/LENSFUN.md; fi
	# A package manager refreshes these caches itself; a plain install must, or Open With misses the app.
	if [ -z "$(DESTDIR)" ]; then update-desktop-database -q $(PREFIX)/share/applications || true; \
	  gtk-update-icon-cache -q -t $(PREFIX)/share/icons/hicolor || true; fi
	if [ -f models/rawnind/bayer.bin ]; then for f in $(MODELS) models/README.md; do \
	  install -Dm644 $$f $(DESTDIR)$(PREFIX)/share/$(BIN)/$$f; done; fi

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BIN) \
	      $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop \
	      $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg \
	      $(foreach f,$(notdir $(wildcard data/icons/hicolor/scalable/actions/*.svg)),$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/actions/$(f)) \
	      $(foreach l,$(LINGUAS),$(DESTDIR)$(PREFIX)/share/locale/$(l)/LC_MESSAGES/$(BIN).mo)
	rm -rf $(DESTDIR)$(PREFIX)/share/licenses/$(BIN) $(DESTDIR)$(PREFIX)/share/$(BIN)

# Packages the committed HEAD (commit first!).
arch:
	git archive --format=tar.gz --prefix=$(BIN)-$(VERSION)/ -o $(ARCHDIR)/$(BIN)-$(VERSION).tar.gz HEAD
	sed -i 's/^pkgver=.*/pkgver=$(VERSION)/' $(ARCHDIR)/PKGBUILD
	cd $(ARCHDIR) && makepkg -sf --needed
	@echo "Install with: sudo pacman -U $(ARCHDIR)/$(BIN)-$(VERSION)-*.pkg.tar.zst"

# Self-contained .app: dylibbundler copies the linked dylibs, and the GTK runtime data
# (schemas, icon themes, pixbuf loaders) comes from Homebrew into the bundle; the
# launcher points GTK at it. Needs Homebrew gtk4, libadwaita, adwaita-icon-theme,
# librsvg and dylibbundler.
APP  := target/RawVibes.app
BREW  = $(shell brew --prefix)
PIXBUF = lib/gdk-pixbuf-2.0/2.10.0
dmg: release models
	rm -rf $(APP) target/dmg target/$(BIN)-$(VERSION).dmg
	install -d $(APP)/Contents/MacOS $(APP)/Contents/libs $(APP)/Contents/Resources $(APP)/Contents/share/glib-2.0/schemas
	install -m755 target/release/$(BIN) $(APP)/Contents/MacOS/$(BIN)-bin
	install -m755 packaging/macos/launcher $(APP)/Contents/MacOS/$(BIN)
	sed 's/@VERSION@/$(VERSION)/g' packaging/macos/Info.plist > $(APP)/Contents/Info.plist
	for f in $(MODELS) models/README.md; do install -d $(APP)/Contents/share/$(BIN)/$$(dirname $$f) && \
	  install -m644 $$f $(APP)/Contents/share/$(BIN)/$$f; done
	for l in $(LINGUAS); do install -d $(APP)/Contents/share/locale/$$l/LC_MESSAGES && \
	  install -m644 target/locale/$$l/LC_MESSAGES/$(BIN).mo $(APP)/Contents/share/locale/$$l/LC_MESSAGES/; done
	install -m644 $(BREW)/share/glib-2.0/schemas/gschemas.compiled $(APP)/Contents/share/glib-2.0/schemas/
	cp -RL $(BREW)/share/icons $(APP)/Contents/share/
	if [ -f target/lensfun.json ]; then install -d $(APP)/Contents/share/$(BIN) && install -m644 target/lensfun.json $(APP)/Contents/share/$(BIN)/; fi
	install -d $(APP)/Contents/share/icons/hicolor/scalable/actions
	install -m644 data/icons/hicolor/scalable/actions/*.svg $(APP)/Contents/share/icons/hicolor/scalable/actions/
	install -d $(APP)/Contents/$(PIXBUF)/loaders
	cp $(BREW)/$(PIXBUF)/loaders/*.so $(APP)/Contents/$(PIXBUF)/loaders/
	# Queried before dylibbundler: afterwards the loaders only load from inside the app.
	gdk-pixbuf-query-loaders $(APP)/Contents/$(PIXBUF)/loaders/*.so \
	  | sed 's|$(abspath $(APP))/Contents|@CONTENTS@|g' > $(APP)/Contents/Resources/loaders.cache.in
	# dylibbundler asks on stdin for a library it cannot find: "quit" fails the build instead of hanging it.
	yes quit | dylibbundler -of -b -s $(BREW)/lib -d $(APP)/Contents/libs -p @executable_path/../libs/ -x $(APP)/Contents/MacOS/$(BIN)-bin \
	  $(foreach f,$(wildcard $(BREW)/$(PIXBUF)/loaders/*.so),-x $(APP)/Contents/$(PIXBUF)/loaders/$(notdir $(f)))
	install -d target/RawVibes.iconset
	for s in 16 32 128 256 512; do \
	  rsvg-convert -w $$s -h $$s data/$(APP_ID).svg -o target/RawVibes.iconset/icon_$${s}x$${s}.png; \
	  rsvg-convert -w $$((s*2)) -h $$((s*2)) data/$(APP_ID).svg -o target/RawVibes.iconset/icon_$${s}x$${s}@2x.png; done
	iconutil -c icns target/RawVibes.iconset -o $(APP)/Contents/Resources/RawVibes.icns
	# dylibbundler's install_name_tool edits break signatures; arm64 refuses to run unsigned code.
	codesign --force --deep -s - $(APP)
	install -d target/dmg
	cp -R $(APP) target/dmg/
	ln -s /Applications target/dmg/Applications
	install -m644 packaging/macos/README.txt target/dmg/README.txt
	hdiutil create -volname RawVibes -srcfolder target/dmg -ov -format UDZO target/$(BIN)-$(VERSION).dmg

clean:
	$(CARGO) clean
	rm -rf $(ARCHDIR)/src $(ARCHDIR)/pkg $(ARCHDIR)/*.tar.gz $(ARCHDIR)/*.pkg.tar.zst
