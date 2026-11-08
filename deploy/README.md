# Image overlay: rustbelt as `pybennu-power-solver`

`simulator: PyPower` scenarios launch `pybennu-power-solver {start|restart} -c /etc/sceptre/config.ini -e pybennu -d`
(sceptre app `sceptre_start.mako`) after `ps cax | grep pybennu`. This overlay makes that command rustbelt.

- `build-overlay.sh` copies `dist/rustbelt` into `overlays/rustbelt/usr/local/bin/`.
- `scripts/rustbelt-power.sh` (chroot step, after `bennu-arm64.sh`) replaces the pip console script with a symlink to it.
- `recipes/bennu-rustbelt-arm64.env` is `bennu-arm64.env` plus those two. Build with `images/build.sh` from the range repo
  using this file (copy or point at it); the image name differs so the stock `bennu-arm64.qc2` is untouched.

Range-side change (not made here): topologies that use the drop-in set the provider host `image: bennu-rustbelt-arm64.qc2`
(for soap-derived topologies: the `power-provider` host and RTUs may stay on `bennu-arm64.qc2`, only the provider needs it).
Nothing else changes: same scenario metadata (`simulator: PyPower`, `case`, `publish_endpoint`).
