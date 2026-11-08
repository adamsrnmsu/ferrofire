#!/bin/sh
# Chroot build step (vmdb runs this under sh -ec: POSIX only), listed AFTER bennu-arm64.sh. The overlays have already
# laid down /usr/local/bin/ferrofire (deploy/overlays/ferrofire) and the pip console script pybennu-power-solver; replace the
# script with a symlink. The name matters twice: the sceptre start script greps `ps cax` for "pybennu" (comm is the
# basename of the exec'd path, so a symlink named pybennu-power-solver shows as pybennu-power-s), and ferrofire picks the
# pybennu-power-solver personality from argv[0]. /usr/bin/pybennu-power-solver (made by bennu-arm64.sh) points here.
set -ex
ln -sf ferrofire /usr/local/bin/pybennu-power-solver
test "$(readlink -f /usr/bin/pybennu-power-solver)" = /usr/local/bin/ferrofire
