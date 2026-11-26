# RawVibes — build, run and package.
#   make run FILE=photo.RW2     debug run, optionally opening a photo
#   make release / run-release  optimised build
#   make qt / run-qt            the Qt interface, optimised; qt-debug / run-qt-debug for a debug build
#   make install PREFIX=~/.local     the GTK interface; install-qt the Qt one (both share install-data)
#   make arch                   build the Arch Linux packages with makepkg (rawvibes, rawvibes-qt, rawvibes-common)
#   make dmg / dmg-qt           build a macOS .dmg, GTK or Qt (run on macOS; dylibbundler / macdeployqt)
#   make zip-qt                 the Qt interface for Windows as a .zip (run in MSYS2 UCRT64, needs windeployqt6)
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

# The binaries are phony: cargo knows when they are stale, make does not.
.PHONY: target/release/$(BIN) target/release/$(BIN)-qt
.PHONY: all build run release run-release qt run-qt qt-debug run-qt-debug test check fmt clean install install-data install-gtk install-qt uninstall arch dmg dmg-qt zip-qt pot locale models lensfun

all: release

build: locale lensfun
	$(CARGO) build

run: locale lensfun
	$(CARGO) run -p $(BIN) -- $(FILE)

release: locale lensfun
	$(CARGO) build --release

run-release: locale lensfun
	$(CARGO) run --release -p $(BIN) -- $(FILE)

qt: locale lensfun
	$(CARGO) build --release -p $(BIN)-qt

run-qt: locale lensfun
	$(CARGO) run --release -p $(BIN)-qt -- $(FILE)

qt-debug: locale lensfun
	$(CARGO) build -p $(BIN)-qt

run-qt-debug: locale lensfun
	$(CARGO) run -p $(BIN)-qt -- $(FILE)

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
LENSFUN_DB ?= $(firstword $(wildcard /usr/share/lensfun/version_1 /opt/homebrew/share/lensfun/version_1 /usr/local/share/lensfun/version_1 /ucrt64/share/lensfun/version_1))
lensfun: target/lensfun.json

target/lensfun.json: tools/lensfun_db.py
	@if [ -n "$(LENSFUN_DB)" ]; then python3 -I tools/lensfun_db.py $(LENSFUN_DB) $@; \
	  else echo "lensfun database not found: no lens profiles"; fi

target/locale/%/LC_MESSAGES/$(BIN).mo: po/%.po
	@mkdir -p $(dir $@)
	msgfmt --check -o $@ $<

pot:
	xgettext -L Rust --from-code=UTF-8 -k --keyword=gettext --keyword='gettext!' \
	  --package-name=RawVibes -o po/$(BIN).pot gtk/src/*.rs qt/src/*.rs qt/src/app/*.rs
	# The Qt interface's QML: Store.tr("…") reads the same catalogue.
	xgettext -L JavaScript --from-code=UTF-8 -k --keyword=tr -j -o po/$(BIN).pot qt/qml/*.qml
	for l in $(LINGUAS); do msgmerge -U --backup=none po/$$l.po po/$(BIN).pot; done

target/release/$(BIN):
	$(CARGO) build --release -p $(BIN)

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

target/release/$(BIN)-qt:
	$(CARGO) build --release -p $(BIN)-qt

install: install-data install-gtk

install-gtk: target/release/$(BIN)
	install -Dm755 target/release/$(BIN) $(DESTDIR)$(PREFIX)/bin/$(BIN)
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	$(REFRESH_CACHES)

install-qt: target/release/$(BIN)-qt
	install -Dm755 target/release/$(BIN)-qt $(DESTDIR)$(PREFIX)/bin/$(BIN)-qt
	install -Dm644 data/$(APP_ID).Qt.desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).Qt.desktop
	$(REFRESH_CACHES)

# A package manager refreshes these caches itself; a plain install must, or Open With misses the app.
REFRESH_CACHES = if [ -z "$(DESTDIR)" ]; then update-desktop-database -q $(PREFIX)/share/applications || true; \
	  gtk-update-icon-cache -q -t $(PREFIX)/share/icons/hicolor || true; fi

# What both interfaces read: translations, icons, the lens database and the AI models
# (when `make models` fetched them: no network here, packagers run it first).
install-data: locale lensfun
	for l in $(LINGUAS); do install -Dm644 target/locale/$$l/LC_MESSAGES/$(BIN).mo \
	  $(DESTDIR)$(PREFIX)/share/locale/$$l/LC_MESSAGES/$(BIN).mo; done
	install -Dm644 data/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 -t $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/actions data/icons/hicolor/scalable/actions/*.svg
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/$(BIN)/LICENSE
	if [ -f target/lensfun.json ]; then install -Dm644 target/lensfun.json $(DESTDIR)$(PREFIX)/share/$(BIN)/lensfun.json; \
	  install -Dm644 LENSFUN.md $(DESTDIR)$(PREFIX)/share/licenses/$(BIN)/LENSFUN.md; fi
	if [ -f models/rawnind/bayer.bin ]; then for f in $(MODELS) models/README.md; do \
	  install -Dm644 $$f $(DESTDIR)$(PREFIX)/share/$(BIN)/$$f; done; fi

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BIN) $(DESTDIR)$(PREFIX)/bin/$(BIN)-qt \
	      $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).Qt.desktop \
	      $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg \
	      $(addprefix $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/actions/,$(notdir $(wildcard data/icons/hicolor/scalable/actions/*.svg))) \
	      $(foreach l,$(LINGUAS),$(DESTDIR)$(PREFIX)/share/locale/$(l)/LC_MESSAGES/$(BIN).mo)
	rm -rf $(DESTDIR)$(PREFIX)/share/licenses/$(BIN) $(DESTDIR)$(PREFIX)/share/$(BIN)

# Packages the committed HEAD (commit first!).
arch:
	git archive --format=tar.gz --prefix=$(BIN)-$(VERSION)/ -o $(ARCHDIR)/$(BIN)-$(VERSION).tar.gz HEAD
	sed -i 's/^pkgver=.*/pkgver=$(VERSION)/' $(ARCHDIR)/PKGBUILD
	cd $(ARCHDIR) && makepkg -sf --needed
	@echo "Install with: sudo pacman -U $(ARCHDIR)/$(BIN)-common-$(VERSION)-*.pkg.tar.zst and $(ARCHDIR)/$(BIN)-$(VERSION)-*.pkg.tar.zst (GTK) or $(ARCHDIR)/$(BIN)-qt-$(VERSION)-*.pkg.tar.zst (Qt)"

# Self-contained .app: dylibbundler copies the linked dylibs, and the GTK runtime data
# (schemas, icon themes, pixbuf loaders) comes from Homebrew into the bundle; the
# launcher points GTK at it. Needs Homebrew gtk4, libadwaita, adwaita-icon-theme, jpeg-xl, cmake, nasm,
# librsvg, lensfun and dylibbundler.
APP  := target/RawVibes.app
BREW  = $(shell brew --prefix)
PIXBUF = lib/gdk-pixbuf-2.0/2.10.0
dmg: release models target/RawVibes.icns
	rm -rf $(APP) target/dmg target/$(BIN)-$(VERSION).dmg
	install -d $(APP)/Contents/MacOS $(APP)/Contents/libs $(APP)/Contents/Resources $(APP)/Contents/share/glib-2.0/schemas
	install -m755 target/release/$(BIN) $(APP)/Contents/MacOS/$(BIN)-bin
	install -m755 packaging/macos/launcher $(APP)/Contents/MacOS/$(BIN)
	$(call plist,RawVibes,$(APP_ID),$(BIN)) > $(APP)/Contents/Info.plist
	$(call share_data,$(APP)/Contents)
	install -m644 $(BREW)/share/glib-2.0/schemas/gschemas.compiled $(APP)/Contents/share/glib-2.0/schemas/
	cp -RL $(BREW)/share/icons $(APP)/Contents/share/
	install -d $(APP)/Contents/share/icons/hicolor/scalable/actions
	install -m644 data/icons/hicolor/scalable/actions/*.svg $(APP)/Contents/share/icons/hicolor/scalable/actions/
	install -d $(APP)/Contents/$(PIXBUF)/loaders
	cp $(BREW)/$(PIXBUF)/loaders/*.so $(APP)/Contents/$(PIXBUF)/loaders/
	# Queried in Homebrew: the copies cannot load yet (librsvg via @rpath) and drop out of the cache.
	gdk-pixbuf-query-loaders $(BREW)/$(PIXBUF)/loaders/*.so \
	  | sed 's|$(BREW)/$(PIXBUF)|@CONTENTS@/$(PIXBUF)|g' > $(APP)/Contents/Resources/loaders.cache.in
	# dylibbundler asks on stdin for a library it cannot find: "quit" fails the build instead of hanging it.
	yes quit | dylibbundler -of -b -s $(BREW)/lib -d $(APP)/Contents/libs -p @executable_path/../libs/ -x $(APP)/Contents/MacOS/$(BIN)-bin \
	  $(foreach f,$(wildcard $(BREW)/$(PIXBUF)/loaders/*.so),-x $(APP)/Contents/$(PIXBUF)/loaders/$(notdir $(f)))
	cp target/RawVibes.icns $(APP)/Contents/Resources/
	# dylibbundler's install_name_tool edits break signatures; arm64 refuses to run unsigned code.
	codesign --force --deep -s - $(APP)
	install -d target/dmg
	cp -R $(APP) target/dmg/
	ln -s /Applications target/dmg/Applications
	install -m644 packaging/macos/README.txt target/dmg/README.txt
	hdiutil create -volname RawVibes -srcfolder target/dmg -ov -format UDZO target/$(BIN)-$(VERSION).dmg

# The Qt interface as a .app: macdeployqt brings Qt's frameworks and plugins, the QML modules
# the UI imports, and every other Homebrew library it links. Needs Homebrew qt, gettext, jpeg-xl,
# cmake, nasm, librsvg and lensfun.
QTAPP := target/RawVibes-Qt.app
dmg-qt: qt models target/RawVibes.icns
	rm -rf $(QTAPP) target/dmg-qt target/$(BIN)-qt-$(VERSION).dmg
	install -d $(QTAPP)/Contents/MacOS $(QTAPP)/Contents/Resources
	install -m755 target/release/$(BIN)-qt $(QTAPP)/Contents/MacOS/$(BIN)-qt
	$(call plist,RawVibes Qt,$(APP_ID).Qt,$(BIN)-qt) > $(QTAPP)/Contents/Info.plist
	cp target/RawVibes.icns $(QTAPP)/Contents/Resources/
	$(call share_data,$(QTAPP)/Contents)
	macdeployqt $(QTAPP) -qmldir=qt/qml
	codesign --force --deep -s - $(QTAPP)
	install -d target/dmg-qt
	cp -R $(QTAPP) target/dmg-qt/
	ln -s /Applications target/dmg-qt/Applications
	sed 's/RawVibes\.app/RawVibes-Qt.app/g' packaging/macos/README.txt > target/dmg-qt/README.txt
	hdiutil create -volname "RawVibes Qt" -srcfolder target/dmg-qt -ov -format UDZO target/$(BIN)-qt-$(VERSION).dmg

# The Qt interface for Windows, from an MSYS2 UCRT64 shell: windeployqt6 brings Qt and the QML
# modules, ldd every other UCRT64 DLL (the MinGW runtime, libjxl, libintl…). Unzip and run
# RawVibes-Qt/bin/rawvibes-qt.exe; the AI models go in models/rawnind first (`make models`).
WINQT := target/RawVibes-Qt
zip-qt: qt
	rm -rf $(WINQT) target/$(BIN)-qt-$(VERSION)-windows.zip
	install -d $(WINQT)/bin/imageformats $(WINQT)/bin/iconengines
	cp target/release/$(BIN)-qt.exe $(WINQT)/bin/
	windeployqt6 --release --qmldir qt/qml --no-translations $(WINQT)/bin/$(BIN)-qt.exe
	# The icons are SVG: Qt's SVG plugins, which no linked module pulls in.
	cp -n "$$(qtpaths6 --query QT_INSTALL_PLUGINS)"/imageformats/qsvg.dll $(WINQT)/bin/imageformats/
	cp -n "$$(qtpaths6 --query QT_INSTALL_PLUGINS)"/iconengines/qsvgicon.dll $(WINQT)/bin/iconengines/
	ldd $(WINQT)/bin/*.exe $$(find $(WINQT)/bin -name '*.dll') | awk '$$3 ~ /^\/ucrt64\// {print $$3}' \
	  | sort -u | xargs -r cp -n -t $(WINQT)/bin
	$(call share_data,$(WINQT))
	install -m644 LICENSE $(WINQT)/LICENSE.txt
	cd target && zip -qr $(BIN)-qt-$(VERSION)-windows.zip RawVibes-Qt

# Info.plist for a bundle: $(1) its name, $(2) its identifier, $(3) its executable.
plist = sed -e 's/@VERSION@/$(VERSION)/g' -e 's/@NAME@/$(1)/g' -e 's/@ID@/$(2)/g' -e 's/@EXEC@/$(3)/g' packaging/macos/Info.plist

# What both interfaces read, into a bundle's share/ under $(1): the AI models (when fetched),
# the translations and the lens database.
define share_data
	for f in $(MODELS) models/README.md; do [ -f $$f ] || continue; install -d $(1)/share/$(BIN)/$$(dirname $$f) && \
	  install -m644 $$f $(1)/share/$(BIN)/$$f; done
	for l in $(LINGUAS); do install -d $(1)/share/locale/$$l/LC_MESSAGES && \
	  install -m644 target/locale/$$l/LC_MESSAGES/$(BIN).mo $(1)/share/locale/$$l/LC_MESSAGES/; done
	if [ -f target/lensfun.json ]; then install -d $(1)/share/$(BIN) && install -m644 target/lensfun.json $(1)/share/$(BIN)/; fi
endef

target/RawVibes.icns: data/$(APP_ID).svg
	install -d target/RawVibes.iconset
	for s in 16 32 128 256 512; do \
	  rsvg-convert -w $$s -h $$s $< -o target/RawVibes.iconset/icon_$${s}x$${s}.png; \
	  rsvg-convert -w $$((s*2)) -h $$((s*2)) $< -o target/RawVibes.iconset/icon_$${s}x$${s}@2x.png; done
	iconutil -c icns target/RawVibes.iconset -o $@

clean:
	$(CARGO) clean
	rm -rf $(ARCHDIR)/src $(ARCHDIR)/pkg $(ARCHDIR)/*.tar.gz $(ARCHDIR)/*.pkg.tar.zst
