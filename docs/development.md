# Development

## Building the CLI

Build with `cargo build -p neko`. On macOS, `neko run` boots the VM through Apple Virtualization, so the binary must be codesigned with the virtualization entitlement:

```
codesign --entitlements neko.entitlements --force -s - target/debug/neko
```

The base VM image (kernel, rootfs, initramfs) is downloaded into the neko data dir on first run, or linked from an existing shuru install if one is present. neko keeps its own data dir (`~/Library/Application Support/neko` on macOS, `NEKO_DATA_DIR` to override), separate from shuru.

## The edge

```
cd edge && bun install && bun test
```

## GitHub rate limits

`neko upgrade` and the first-run image download read GitHub, which allows 60 anonymous calls an hour per address. Pass a token if you hit that:

```sh
NEKO_GITHUB_TOKEN=ghp_... neko upgrade
```
