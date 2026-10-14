# Contributing to oak-cam

Thanks for your interest. oak-cam turns a Luxonis OAK-D Pro (USB or PoE) into
three OS virtual webcams on macOS, Linux and Windows 11.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
Harassment of any kind is not tolerated. Report incidents to
grant@surlysoft.com.

## Reporting bugs

Open a GitHub issue with:

- OS and version, and whether the device is USB or PoE.
- `oak-cam --version` and `oak-cam --list` output.
- What you did, what you expected, what happened. Attach logs run with
  `RUST_LOG=debug DEPTHAI_LEVEL=debug`.

## Proposing changes

1. Open an issue first for anything larger than a small fix, so the approach
   can be agreed before you spend time on it. `FUTURE_FEATURES.md` holds
   unscheduled ideas.
2. Fork, then branch from `main` (`<area>/<topic>`, for example `vcam/linux-fps`).
3. Keep pull requests focused. One change per PR.
4. Before pushing, make sure these pass:

   ```
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

   Tests that need an OAK device are `#[ignore]`; run them with
   `cargo test --workspace -- --ignored` when you have hardware attached and
   say which device you tested with in the PR.
5. A maintainer reviews and merges. Merges into `main` are done with
   `--no-ff` so each change stays a visible unit.

## Building

depthai-core v3.10.0 is built once from source:

```
scripts/depthai/build.sh        # macOS / Linux, 20 to 60 minutes cold
```

It installs to `~/.cache/oak-cam/depthai/<tag>` and the workspace picks it up
from the `current` symlink there. After that, `cargo build` works as usual.

## Style

- Rust 2024 edition, toolchain pinned in `rust-toolchain.toml`. Clippy
  pedantic is on; allow a lint only with a comment saying why.
- All C++ lives in `crates/depthai-sys/csrc`. A change to the C ABI updates
  `oak_dai.h`, `oak_dai.cpp` and `src/lib.rs` together, and the signature
  list in `csrc/README.md`.
- Commit messages: a short imperative summary line, then a body explaining
  why when the diff does not make it obvious.

## License

By contributing you agree that your contributions are licensed under the
project's [MIT license](LICENSE). Third-party notices live in
`THIRD_PARTY_LICENSES.md`.
