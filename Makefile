PREFIX ?= /usr/local
DESTDIR ?=
BIN = $(DESTDIR)$(PREFIX)/bin
SESSIONS = $(DESTDIR)/usr/share/wayland-sessions

.PHONY: install uninstall

install:
	@test -x target/release/jff || { echo "run 'cargo build --release' first (as your user)"; exit 1; }
	install -Dm755 target/release/jff $(BIN)/jff
	install -Dm755 resources/jff-session $(BIN)/jff-session
	mkdir -p $(SESSIONS)
	sed 's|@PREFIX@|$(PREFIX)|g' resources/jff.desktop.in > $(SESSIONS)/jff.desktop
	chmod 644 $(SESSIONS)/jff.desktop
	@echo "installed: jff now shows up in the session list of your login manager"

uninstall:
	rm -f $(BIN)/jff $(BIN)/jff-session $(SESSIONS)/jff.desktop
