# flannel-ng

## Overview
flannel-ng is a rust implementation of the [Flannel](https://github.com/flannel-io/flannel) network overlay for container. This project is currently for study and research purpose.

## Backend support

Support full-featured udp backend, other backends will be added in the future.

- [x] UDP
- [x] VXLAN
- [x] Host-GW

## Getting Started

Usage is same to the original flannel, you can refer to the [flannel](https://github.com/flannel-io/flannel) project for more details.

### Installation
```bash
cargo build --release
./flannel-ng --etcd-endpoints=[YourEtcdEndpoints]
```

## Licensing

Flannel is under the Apache 2.0 license. See the [LICENSE][license] file for details.