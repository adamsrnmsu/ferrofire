# Profiling harness (ww-1df.34)

The scripts used for [docs/profiling.md](../../docs/profiling.md), copied as they ran. They hardcode the author's
paths (`/Users/ryanadams/git/ferrofire`, scratch dir `/var/tmp/ff-prof`, with `scripts/` = this directory and
`img/Dockerfile.2204` = `Dockerfile.perf`), so edit `lib.sh` first. `patches/` holds the prototype diffs
(A-F) from the measurement campaign; none is merged. Run one heavy container at a time.
