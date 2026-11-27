# Distro packaging

The Arch and Void recipes build the release tag pinned in each recipe and use
the top-level `make install` target. Both currently target
[`v0.7.0`](https://github.com/dani-77/spitfire/releases/tag/v0.7.0).

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

The Void `version` and Arch `pkgver` fields select the release tag. When
cutting a release, update both and recompute the tarball checksum for
`checksum` and `sha256sums` with this command.

```sh
curl -sL https://github.com/dani-77/spitfire/archive/refs/tags/vX.Y.Z.tar.gz | sha256sum
```
