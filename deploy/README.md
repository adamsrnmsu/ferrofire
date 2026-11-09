# Image overlay: ferrofire as `pybennu-power-solver`

`simulator: PyPower` scenarios launch `pybennu-power-solver {start|restart} -c /etc/sceptre/config.ini -e pybennu -d`
(sceptre app `sceptre_start.mako`) after `ps cax | grep pybennu`. This overlay makes that command ferrofire.

- `build-overlay.sh [zmq|nng]` copies `dist/ferrofire` into `overlays/ferrofire/usr/local/bin/` (or `overlays/ferrofire-nng/...`),
  after checking the binary matches the variant (the nng image's must link no libzmq).
- `scripts/ferrofire-power.sh` (chroot step, after `bennu-arm64.sh`) replaces the pip console script with a symlink to it.
- `recipes/bennu-ferrofire-arm64.env` is `bennu-arm64.env` plus those two. Build with `images/build.sh` from the range repo
  using this file (copy or point at it); the image name differs so the stock `bennu-arm64.qc2` is untouched.

Range-side change (not made here): topologies that use the drop-in set the provider host `image: bennu-ferrofire-arm64.qc2`
(for soap-derived topologies: the `power-provider` host and RTUs may stay on `bennu-arm64.qc2`, only the provider needs it).
Nothing else changes: same scenario metadata (`simulator: PyPower`, `case`, `publish_endpoint`).

## Two images, one per transport (provider-spec 11)

| Image | Recipe | ferrofire build | Transport |
|---|---|---|---|
| `bennu-ferrofire-arm64` | `recipes/bennu-ferrofire-arm64.env` | default features (`./build-linux.sh`), or `--features zmq` | zmq: talks to stock bennu field devices and bennu-probe |
| `bennu-ferrofire-nng-arm64` | `recipes/bennu-ferrofire-nng-arm64.env` | `CARGO_FLAGS="--no-default-features --features nng" ./build-linux.sh`, then `build-overlay.sh nng` | nng: REP0 + PUB0; talks to NNG clients only |

"No libzmq" means the ferrofire binary links none (`readelf -d` shows only libgcc_s, libm, libc). The nng image is still built on
`bennu-arm64.sh`, so stock bennu and its libzmq are in the image; ferrofire just does not use them.

Stock bennu field devices speak ZeroMQ only (RADIO/DISH for updates), so they cannot talk to the nng image. The nng image's
`pybennu-power-solver` needs `transport = nng` in `config.ini` (the default is zmq, which an nng-only binary refuses at
startup, naming the feature); `server-endpoint` / `publish-endpoint` are then NNG URLs such as `tcp://0.0.0.0:5555` / `tcp://0.0.0.0:5556`.
