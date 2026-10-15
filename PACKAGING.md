# Packaging Weir (.deb)

## Requirements
- Rust toolchain
- `cargo-deb` (`cargo install cargo-deb`)

## Build and package

```bash
scripts/build-deb.sh
```

The script builds the current tree, runs `cargo deb`, then increments the build
number (`revision` in `Cargo.toml` and `BUILD_NUMBER` in `src/cli.rs`, which must
match) and commits that packaging-only bump. It refuses to run if `Cargo.toml` or
`src/cli.rs` has uncommitted changes. Semantic version changes are made with the
source changes, before packaging.

To cut a release, change `version` in `Cargo.toml`, commit it, tag that commit
(`git tag -a vX.Y.Z`), push the commit and the tag, and then build the package.
The tag, the version in `Cargo.toml` and `weir --version` should all agree.

The package is created under `target/debian/*.deb`.

## Release workflow

Pushing a tag that starts with `v` runs `.github/workflows/release.yml`. It checks that the tag
matches `version` in `Cargo.toml`, runs the tests, builds the package with `cargo deb`, and attaches
the `.deb` and a `SHA256SUMS` file to a GitHub release for that tag. It can also be started by hand,
which builds the package and keeps it as a workflow artifact without publishing a release.

The package is built on `ubuntu-latest`, so it depends on that image's glibc (currently
`libc6 >= 2.39`) and won't install on older distributions.

## Install

```bash
sudo apt install ./target/debian/*.deb
```

## Notes
- The binary is installed to `/usr/bin/weir`; `weir --version` prints `version-build`.
- Docs and the example scripts are installed under `/usr/share/doc/weir/`.
- The binary links the host libc dynamically (see `docs/IMPLEMENTATION.md`).
