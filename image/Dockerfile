# Buildroot build host and QEMU smoke host for the RTU image (image/build.sh, image/test/smoke.sh).
FROM debian:bookworm
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential bc bzip2 ca-certificates cpio file git libncurses-dev patch perl python3 rsync \
      unzip wget xz-utils \
      fdisk qemu-utils qemu-system-arm qemu-efi-aarch64 openssh-client sshpass \
    && rm -rf /var/lib/apt/lists/*
