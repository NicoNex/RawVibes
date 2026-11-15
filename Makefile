# RawValley — build, run and package.
#   make run FILE=photo.RW2     debug run, optionally opening a photo
#   make release / run-release  optimised build
#   make install PREFIX=~/.local
#   make arch                   build an Arch Linux package with makepkg
#   make dmg                    build a macOS .dmg (run on macOS, needs dylibbundler)
#   make pot                    refresh po/rawvalley.pot and merge it into po/*.po
#   make models                 fetch and convert the AI denoise weights (needs curl, unzip, python3 + onnx, numpy)

PREFIX  ?= /usr/local
DESTDIR ?=
CARGO   ?= cargo
FILE    ?=

APP_ID  := io.github.rawvalley.RawValley
BIN     := rawvalley
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCHDIR := packaging/arch
LINGUAS := $(basename $(notdir $(wildcard po/*.po)))
MOS     := $(foreach l,$(LINGUAS),target/locale/$(l)/LC_MESSAGES/$(BIN).mo)
# RawNIND UtNet2 as darktable ships it (GPL-3.0 + CC-BY-4.0 weights, see models/README.md).
MODELS_URL := https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/rawdenoise-nind.dtmodel
MODELS_SHA := d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af
MODELS     := $(foreach v,bayer linear,models/rawnind/$(v).bin models/rawnind/$(v).json)
DTMODEL    := target/models/rawdenoise-nind.dtmodel

.PHONY: all build run release run-release test check fmt clean install uninstall arch dmg pot locale models

all: release

build: locale
	$(CARGO) build

run: locale
	$(CARGO) run -p $(BIN) -- $(FILE)

release: locale
	$(CARGO) build --release

run-release: locale
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

target/locale/%/LC_MESSAGES/$(BIN).mo: po/%.po
	@mkdir -p $(dir $@)
	msgfmt --check -o $@ $<

pot:
	xgettext -L Rust --from-code=UTF-8 -k --keyword=gettext --keyword='gettext!' \
	  --package-name=RawValley -o po/$(BIN).pot gtk/src/*.rs
	for l in $(LINGUAS); do msgmerge -U --backup=none po/$$l.po po/$(BIN).pot; done

target/release/$(BIN):
	$(CARGO) build --release

models: $(MODELS)

# Downloaded unless already there (the PKGBUILD puts makepkg's copy there).
$(DTMODEL):
	mkdir -p target/models
	curl -L --fail -o $@.part $(MODELS_URL)
	mv $@.part $@

# One run writes every file; a failed one leaves none behind to look done.
$(MODELS) &: $(DTMODEL)
	echo "$(MODELS_SHA)  $(DTMODEL)" | sha256sum -c
	unzip -o -q $(DTMODEL) -d target/models
	python3 tools/convert_rawnind.py target/models/rawdenoise-nind models/rawnind || { rm -rf models/rawnind; exit 1; }

# Installs the AI models when `make models` fetched them (no network here: packagers run it first).
install: target/release/$(BIN) locale
	install -Dm755 target/release/$(BIN) $(DESTDIR)$(PREFIX)/bin/$(BIN)
	for l in $(LINGUAS); do install -Dm644 target/locale/$$l/LC_MESSAGES/$(BIN).mo \
	  $(DESTDIR)$(PREFIX)/share/locale/$$l/LC_MESSAGES/$(BIN).mo; done
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/$(BIN)/LICENSE
	if [ -f models/rawnind/bayer.bin ]; then for f in $(MODELS) models/README.md; do \
	  install -Dm644 $$f $(DESTDIR)$(PREFIX)/share/$(BIN)/$$f; done; fi

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BIN) \
	      $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop \
	      $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg \
	      $(foreach l,$(LINGUAS),$(DESTDIR)$(PREFIX)/share/locale/$(l)/LC_MESSAGES/$(BIN).mo)
	rm -rf $(DESTDIR)$(PREFIX)/share/licenses/$(BIN) $(DESTDIR)$(PREFIX)/share/$(BIN)

# Packages the committed HEAD (commit first!).
arch:
	git archive --format=tar.gz --prefix=$(BIN)-$(VERSION)/ -o $(ARCHDIR)/$(BIN)-$(VERSION).tar.gz HEAD
	sed -i 's/^pkgver=.*/pkgver=$(VERSION)/' $(ARCHDIR)/PKGBUILD
	cd $(ARCHDIR) && makepkg -f
	@echo "Install with: sudo pacman -U $(ARCHDIR)/$(BIN)-$(VERSION)-*.pkg.tar.zst"

# ponytail: dylibbundler copies the linked dylibs only; GTK runtime data (pixbuf loaders,
# Adwaita icon theme, schemas) still comes from Homebrew. Switch to gtk-mac-bundler if the
# .app must run on Macs without Homebrew GTK.
APP := target/RawValley.app
dmg: release models
	rm -rf $(APP) target/$(BIN)-$(VERSION).dmg
	install -d $(APP)/Contents/MacOS $(APP)/Contents/libs
	install -m755 target/release/$(BIN) $(APP)/Contents/MacOS/$(BIN)
	for f in $(MODELS) models/README.md; do install -d $(APP)/Contents/share/$(BIN)/$$(dirname $$f) && \
	  install -m644 $$f $(APP)/Contents/share/$(BIN)/$$f; done
	for l in $(LINGUAS); do install -d $(APP)/Contents/share/locale/$$l/LC_MESSAGES && \
	  install -m644 target/locale/$$l/LC_MESSAGES/$(BIN).mo $(APP)/Contents/share/locale/$$l/LC_MESSAGES/; done
	printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' \
	  '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
	  '<plist version="1.0"><dict>' \
	  '<key>CFBundleName</key><string>RawValley</string>' \
	  '<key>CFBundleIdentifier</key><string>$(APP_ID)</string>' \
	  '<key>CFBundleExecutable</key><string>$(BIN)</string>' \
	  '<key>CFBundleShortVersionString</key><string>$(VERSION)</string>' \
	  '<key>CFBundlePackageType</key><string>APPL</string>' \
	  '<key>NSHighResolutionCapable</key><true/>' \
	  '</dict></plist>' > $(APP)/Contents/Info.plist
	dylibbundler -of -b -x $(APP)/Contents/MacOS/$(BIN) -d $(APP)/Contents/libs -p @executable_path/../libs/
	# dylibbundler's install_name_tool edits break signatures; arm64 refuses to run unsigned code.
	codesign --force --deep -s - $(APP)
	hdiutil create -volname RawValley -srcfolder $(APP) -ov -format UDZO target/$(BIN)-$(VERSION).dmg

clean:
	$(CARGO) clean
	rm -rf $(ARCHDIR)/src $(ARCHDIR)/pkg $(ARCHDIR)/*.tar.gz $(ARCHDIR)/*.pkg.tar.zst
