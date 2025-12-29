# Container file filesystem

## Overview

This folder contains the filesystem layout for the container.

Container will mount overlayfs with the lowerdir=rootfs plus any directory named layerXX in order of increasing number. The upperdir will be fs/upper and
the workdir will be fs/workdir.

The resulting overlay filesystem will be mounted at /fs/merged

## Filesystem layers

- `rootfs` contains Alpine Linux rootfs for x86_64 architecture.
- `layer01` contains statically compiled for x86_64 architecture fuse-overlayfs binary in /usr/bin and will be mounted on top of rootfs
- `upper` and `work` are empty directories that will be used as upperdir and workdir for overlayfs respectively. After a run `upper` folder can be copied to `layerXX` to
  create a new fs layer.
- `merged` is the mount point for the overlayfs filesystem.
