# Image overlay: ferrofire as `pybennu-power-solver`

`simulator: PyPower` scenarios launch `pybennu-power-solver {start|restart} -c /etc/sceptre/config.ini -e pybennu -d`
(sceptre app `sceptre_start.mako`) after `ps cax | grep pybennu`. This overlay makes that command ferrofire.

- `build-overlay.sh` copies `dist/ferrofire` into `overlays/ferrofire/usr/local/bin/`.
- `scripts/ferrofire-power.sh` (chroot step, after `bennu-arm64.sh`) replaces the pip console script with a symlink to it.
- `recipes/bennu-ferrofire-arm64.env` is `bennu-arm64.env` plus those two. Build with `images/build.sh` from the range repo
  using this file (copy or point at it); the image name differs so the stock `bennu-arm64.qc2` is untouched.

Range-side change (not made here): topologies that use the drop-in set the provider host `image: bennu-ferrofire-arm64.qc2`
(for soap-derived topologies: the `power-provider` host and RTUs may stay on `bennu-arm64.qc2`, only the provider needs it).
Nothing else changes: same scenario metadata (`simulator: PyPower`, `case`, `publish_endpoint`).
