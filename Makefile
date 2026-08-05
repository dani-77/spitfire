PREFIX := /usr

.PHONY: build install uninstall

build:
	cargo build --release

# NOTE: the .desktop session entry only makes sense once spitfire has a
# DRM/KMS backend (Phase 7, out of scope for now) — `spitfire` with no
# args runs the --winit backend, which needs an existing host Wayland/X11
# session to nest into, so a display manager launching it from a bare TTY
# would fail. Installed anyway so packaging is ready the day that lands;
# until then, run `cargo run -p spitfire -- --winit` from inside your
# current session instead of selecting it at the login screen.
install: build
	install -Dm755 target/release/spitfire $(DESTDIR)$(PREFIX)/bin/spitfire
	install -Dm755 target/release/spitfirectl $(DESTDIR)$(PREFIX)/bin/spitfirectl
	install -Dm644 packaging/spitfire.desktop $(DESTDIR)$(PREFIX)/share/wayland-sessions/spitfire.desktop
	install -Dm644 assets/logo/spitfire-wc-icon.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/spitfire.svg
	install -Dm644 assets/logo/spitfire-wc-icon-16.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/16x16/apps/spitfire.png
	install -Dm644 assets/logo/spitfire-wc-icon-32.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/32x32/apps/spitfire.png
	install -Dm644 assets/logo/spitfire-wc-icon-192.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/192x192/apps/spitfire.png
	install -Dm644 assets/logo/spitfire-wc-icon-512.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/512x512/apps/spitfire.png

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/spitfire
	rm -f $(DESTDIR)$(PREFIX)/bin/spitfirectl
	rm -f $(DESTDIR)$(PREFIX)/share/wayland-sessions/spitfire.desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/spitfire.svg
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/16x16/apps/spitfire.png
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/32x32/apps/spitfire.png
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/192x192/apps/spitfire.png
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/512x512/apps/spitfire.png
