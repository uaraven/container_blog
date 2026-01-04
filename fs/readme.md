# Container file filesystem

## Overview

This folder contains the filesystem layout for the container.

Container will mount overlayfs with the lowerdir=rootfs plus any directory named layerXX in order of increasing number.

`fs/upper` must exist and be empty - it will be used as upperdir for overlayfs. `fs/workdir` will be created to be used
as a work directory. If workdir exists and is not empty it will be deleted and recreated.

The resulting overlay filesystem will be mounted at `fs/merged`. This directory will be created automatically. If it exists it will be deleted and recreated.

## Filesystem layers

- `rootfs` contains Alpine Linux rootfs for x86_64 architecture.
- `layer01` contains /etc/resolv.conf file with localhost as a DNS server.
- `upper` is an empty directory that will be used as upperdir for overlayfs. After a run `upper` folder can be copied to `layerXX` to create a new fs layer.
