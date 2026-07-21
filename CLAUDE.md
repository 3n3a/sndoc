# sndoc

## Versioning

For minor changes and patch-level fixes, bump the crate version with `cargo bump minor` or `cargo bump patch` (the `cargo-bump` subcommand — `cargo install cargo-bump` if missing) rather than hand-editing `version` in `Cargo.toml`.

- `cargo bump minor` — new features / user-visible additions (backwards-compatible)
- `cargo bump patch` — pure bug fixes
- After bumping, run `cargo build` so `Cargo.lock`'s version field stays in sync (see commit `3091e10 fix(ci): sync Cargo.lock with bumped version` for the precedent)
- Skip bumping for non-functional changes (comments, formatting) unless asked
