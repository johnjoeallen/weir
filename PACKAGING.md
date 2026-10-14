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

## Install

```bash
sudo apt install ./target/debian/*.deb
```

## Notes
- The binary is installed to `/usr/bin/weir`; `weir --version` prints `version-build`.
- Docs and the example scripts are installed under `/usr/share/doc/weir/`.
- The binary links the host libc dynamically (see `docs/IMPLEMENTATION.md`).
