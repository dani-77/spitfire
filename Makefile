PREFIX := /usr

.PHONY: build install uninstall

# udev (DRM/KMS) is what the .desktop entry below actually needs — it's
# opt-in at the cargo level (see crates/spitfire/Cargo.toml) so a plain
# `cargo build` stays lightweight for --winit-only development, but an
# installed, login-screen-selectable spitfire needs it. xwayland comes
# along too: X11 app support is cheap to include and there's no good
# reason an installed build shouldn't have it.
build:
	cargo build --release --features udev,xwayland

# The .desktop session entry launches packaging/spitfire-session, a thin
# wrapper (not `spitfire --udev` directly) so its stderr/stdout ends up
# somewhere findable ($XDG_STATE_HOME/spitfire/session.log) instead of
# wherever the display manager sends an Exec= command's output by
# default — see that script for why.
install: build
	install -Dm755 target/release/spitfire $(DESTDIR)$(PREFIX)/bin/spitfire
	install -Dm755 target/release/spitfirectl $(DESTDIR)$(PREFIX)/bin/spitfirectl
	install -Dm755 packaging/spitfire-session $(DESTDIR)$(PREFIX)/bin/spitfire-session
	install -Dm644 packaging/spitfire.desktop $(DESTDIR)$(PREFIX)/share/wayland-sessions/spitfire.desktop
	install -Dm644 packaging/spitfire-portals.conf $(DESTDIR)/etc/xdg-desktop-portal/spitfire-portals.conf
	install -Dm644 assets/logo/spitfire-wc-icon.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/spitfire.svg
	install -Dm644 assets/logo/spitfire-wc-icon-16.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/16x16/apps/spitfire.png
	install -Dm644 assets/logo/spitfire-wc-icon-32.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/32x32/apps/spitfire.png
	install -Dm644 assets/logo/spitfire-wc-icon-192.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/192x192/apps/spitfire.png
	install -Dm644 assets/logo/spitfire-wc-icon-512.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/512x512/apps/spitfire.png

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/spitfire
	rm -f $(DESTDIR)$(PREFIX)/bin/spitfirectl
	rm -f $(DESTDIR)$(PREFIX)/bin/spitfire-session
	rm -f $(DESTDIR)$(PREFIX)/share/wayland-sessions/spitfire.desktop
	rm -f $(DESTDIR)/etc/xdg-desktop-portal/spitfire-portals.conf
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/spitfire.svg
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/16x16/apps/spitfire.png
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/32x32/apps/spitfire.png
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/192x192/apps/spitfire.png
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/512x512/apps/spitfire.png
