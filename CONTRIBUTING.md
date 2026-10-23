# Contributing

Thanks for having a look. NoteCraft is a small, deliberately dependency-light Rust app; the
fastest way to get a change in is to keep it that way.

## Before you start

- **Bug reports** are most useful with the platform, the version (`notecraft --version`), what you
  did, and what happened instead. If it is a drawing/layout problem, a screenshot with the thing
  circled beats a description.
- **Feature requests**: open an issue first. This is a notes app with one opinionated design;
  a large new surface needs agreement before code.
- **Security**: please email the maintainer rather than opening a public issue.

## Working on the code

```sh
cargo build            # debug build
cargo test             # unit tests; no display or network needed
cargo run -- --version
```

- `cargo test` must stay green and warning-free (`cargo build` should print no warnings). The one
  test that shells out (PDF export) skips itself when no renderer is installed.
- Everything platform-specific goes in `src/platform.rs` behind a `cfg`, or in a `cfg`-gated entry
  point (`dragout.rs` is Linux/X11 only). Do not put `#[ cfg(windows) ]` blocks into `app.rs` —
  the point of `platform.rs` is that the rest of the app does not know what it is running on.
- If you add a dependency, say why in a comment above it. Fonts, icons and dictionaries are
  compiled in; keep the binary self-contained.
- Formatting style is the one already in the file: 100-column comments that explain *why*, not
  *what*. Do not run `cargo fmt` over the whole tree — it rewrites the layout the drawing code
  depends on.

## Testing a change

There is no automated UI test, so:

1. `cargo test` — the data layer (vault, document round trip, todos, exports) is covered there.
2. Run the app against a scratch vault, never your real notes:
   ```sh
   NOTECRAFT_VAULT=/tmp/notecraft-test cargo run
   ```
3. On Linux, remember that dragging *in* only works on the X11 backend: `cargo run -- --x11`.

## Releasing

Maintainer only:

```sh
# bump the version in Cargo.toml (and Cargo.lock: `cargo build` updates it)
git commit -am "vX.Y.Z"
git tag vX.Y.Z
git push origin main --tags
```

The `Release` workflow builds the Linux tarball and `.deb`, the macOS `.dmg` per architecture, the
Windows `.zip`, and attaches them plus `.sha256` files to the GitHub release.
