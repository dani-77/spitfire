# Distro packaging

Both of these build tag [`v0.4.0`](https://github.com/dani-77/spitfire/releases/tag/v0.4.0)
from source and simply call the project's own `make install` (see the top-level
`Makefile`) for the actual install step, so the file list here doesn't drift from
what `sudo make install` already does.

## Arch Linux

`arch/PKGBUILD` — a standard AUR-style `PKGBUILD`. To build and install locally:

```sh
cd packaging/arch
makepkg -si
```

If you're submitting this to the AUR, regenerate `.SRCINFO` first:

```sh
makepkg --printsrcinfo > .SRCINFO
```

## Void Linux

`void/spitfire/template` — a `void-packages` template. Drop it into a local
`void-packages` checkout and build with `xbps-src`:

```sh
cp -r packaging/void/spitfire /path/to/void-packages/srcpkgs/
cd /path/to/void-packages
./xbps-src pkg spitfire
```

## Bumping the version

Both files pin `version`/`pkgver` to `0.4.0` and a `checksum`/`sha256sums` for that
tag's release tarball. When cutting a new release, update both and recompute the
checksum, e.g.:

```sh
curl -sL https://github.com/dani-77/spitfire/archive/refs/tags/vX.Y.Z.tar.gz | sha256sum
```
