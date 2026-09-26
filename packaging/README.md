# Distro packaging

Both of these build tag [`v0.5.1`](https://github.com/dani-77/spitfire/releases/tag/v0.5.1)
from source and simply call the project's own `make install` (see the top-level
`Makefile`) for the actual install step, so the file list here doesn't drift from
what `sudo make install` already does.

## Arch Linux

`arch/PKGBUILD` — the same `PKGBUILD` published in the AUR as
[`spitfire`](https://aur.archlinux.org/packages/spitfire):

```sh
yay -S spitfire
# or, from this repo:
cd packaging/arch
makepkg -si
```

## Void Linux

`void/spitfire/template` — a copy of the template in
[`d77void/srcpkgs-d77`](https://github.com/d77void/srcpkgs-d77). Drop it into a local
`void-packages` checkout and build with `xbps-src`:

```sh
cp -r packaging/void/spitfire /path/to/void-packages/srcpkgs/
cd /path/to/void-packages
./xbps-src pkg spitfire
```

## Bumping the version

Both files pin `version`/`pkgver` to `0.5.1` and a `checksum`/`sha256sums` for that
tag's release tarball. When cutting a new release, update both and recompute the
checksum, e.g.:

```sh
curl -sL https://github.com/dani-77/spitfire/archive/refs/tags/vX.Y.Z.tar.gz | sha256sum
```
