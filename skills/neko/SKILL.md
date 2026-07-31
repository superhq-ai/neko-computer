---
name: neko
description: Expose a sandbox or a local port to the public web with the neko CLI, at a <name>.neko.computer https URL. Use when the user wants a public link for a server, to share a running app or demo, to run a command in a fresh sandbox and tunnel its port, or to manage reusable sandbox checkpoints.
---

# Public Tunnels and Sandboxes with neko

neko puts a port on the internet with one command. It has two modes:

- `neko run ... --port PORT` boots a sandbox and forwards its port to 127.0.0.1 on this machine. Local only, no account needed. Use this for development.
- `neko tunnel PORT` shares a port already running on the host (127.0.0.1:PORT) on the public web.
- `neko run ... --tunnel PORT` boots a fresh shuru microVM sandbox, runs a command in it, and shares the port the command listens on publicly.

Public tunnels (the last two) need SuperHQ Pro; a free account is refused at connect with an upgrade link. `--port` is free.

Both give a public URL at `<subdomain>.neko.computer`. A subdomain is generated (like `swift-otter-h7kq`) unless you pass `--subdomain`. A chosen subdomain is claimed by your account on first use and stays yours until it goes unused for 30 days; accounts can hold a limited number of names. Generated names are free again as soon as the tunnel closes. A name owned by another account is refused with a clear error.

## Setup

Sign in once. Opening a public tunnel needs a SuperHQ Pro account; `--port`
forwarding does not.

```bash
neko login
```

## Develop against a sandbox (free, no account)

Boot a sandbox, run a server in it, and reach it from this machine.

```bash
neko run web --port 8000 -- python3 -m http.server 8000
# -> forwarding http://127.0.0.1:8000 -> sandbox:8000

neko run web --port 3000:8000 -- python3 -m http.server 8000   # different host port
```

## Share a local port (no sandbox)

The server is already running on the host. This works on any platform.

```bash
# terminal already running: npm run dev on :3000
neko tunnel 3000
# -> (=^..^=)つ tunnel open: https://swift-otter-h7kq.neko.computer -> 127.0.0.1:3000 (public)

neko tunnel 3000 --subdomain my-demo
# -> https://my-demo.neko.computer -> 127.0.0.1:3000
```

Leave it running; press ctrl-c to close the tunnel.

## Run in a sandbox and share it

`neko run` boots an isolated Linux microVM, mounts the current directory read-only at `/workspace`, runs the command there, and streams output. With `--tunnel` it also exposes the guest port to a public URL. Requires an aarch64 host (macOS on Apple Silicon, or Linux).

```bash
# anonymous, ephemeral sandbox: serve the current dir and share it
neko run --tunnel 8000 -- python3 -m http.server 8000
```

The base image is a minimal Debian aarch64 with no language runtime, so the command must install what it needs — and installing needs `--allow-net`, since the sandbox has no network access by default. neko runs the command as argv directly, with no shell, so shell operators like `&&` need an explicit `sh -c`:

```bash
neko run --allow-net --tunnel 8000 -- sh -c 'apt-get update && apt-get install -y python3 && python3 -m http.server 8000'
```

## The sandbox starts closed

Two defaults differ from what a command usually expects. Both are printed at boot, so read that output before diagnosing a failure.

```
mounted /Users/you/project at /workspace (read-only; writes stay in the sandbox)
networking: off (--allow-net to enable)
```

- **No network.** The guest boots with no network device, so `apt-get`, `npm install`, `pip`, and any outbound request fail. Pass `--allow-net` for open egress, or `--allow-host PATTERN` (repeatable, implies `--allow-net`) to allow only certain hosts, e.g. `--allow-host '*.npmjs.org'`. This does **not** affect `--tunnel`, `--port`, or `--term`: those reach the guest over vsock and work with networking off.
- **The cwd mount is read-only, but writes still succeed.** The guest puts an overlay over the share, so `npm install`, build output, and generated files all work — they land in a scratch layer that is discarded when the sandbox exits (a `--checkpoint` does not keep it either). The user's real directory is never modified. Pass `--write` only when the point is to produce files on the host, e.g. a build whose output the user wants to keep.

So a command that installs deps and serves needs `--allow-net` (or `--allow-host`) but usually not `--write`.

## Computers and checkpoints (instant boot)

Installing a runtime on every run is slow. Provision it once, snapshot the disk, then boot from the snapshot instantly.

A **computer** is a named, versioned sandbox (a branch). A **checkpoint** is an immutable disk snapshot (a commit). `--checkpoint [LABEL]` snapshots on the way down; `--from REF` boots from a checkpoint.

```bash
# 1. provision once into a computer named "web", label the snapshot "ready"
neko run web --allow-net --checkpoint ready -- sh -c 'apt-get update && apt-get install -y python3'

# 2. boot from that checkpoint instantly and serve, sharing the port
neko run web --from web@ready --tunnel 8000 -- python3 -m http.server 8000

# resuming a named computer picks up its latest head automatically
neko run web --tunnel 8000 -- python3 -m http.server 8000
```

`--from` takes `name` (the computer head) or `name@label` (a specific checkpoint).

## Managing computers

```bash
neko ls                          # list computers
neko history web                 # checkpoint history of a computer
neko clone web@ready staging     # new computer from a ref (shared node) or an image file
neko rm web                      # remove a computer, reclaim its orphaned checkpoints
neko rm web --keep-checkpoints   # remove the computer but keep its checkpoints
neko gc                          # prune checkpoints unreachable from any computer
```

## neko run flags

```bash
neko run [NAME] [flags] -- COMMAND

--port [HOST:]GUEST  forward a guest port to 127.0.0.1 here (free, repeatable)
--tunnel PORT        expose the guest port to a public https URL (needs Pro)
--subdomain SUB      request a specific subdomain instead of a generated one
--private [SLUG]     only workspace members may visit (bare = personal workspace)
--term               web terminal into the sandbox at /__neko/term (opens its own
                     private tunnel; combine with --tunnel to also expose a port)
--allow-net          let the sandbox reach the network (off by default)
--allow-host PAT     allow only these hosts, e.g. '*.npmjs.org' (repeatable,
                     implies --allow-net)
--write              mount the cwd read-write (read-only by default)
--mount SPEC         mount a host dir: HOST:GUEST[:ro|:rw], read-only by default.
                     Repeatable; replaces the default cwd mount
--workdir PATH       where the command runs (default: first mount's guest path)
--from REF           boot from a checkpoint (name or name@label)
--checkpoint [LABEL] snapshot the disk on exit, optionally labelled
```

Omit NAME for an anonymous, ephemeral sandbox (discarded on exit). Pass NAME to persist it as a computer.

## Other commands

```bash
neko upgrade    # update neko to the latest release
```

## Common patterns

### Share a dev server from your machine

```bash
# server already running locally on :5173
neko tunnel 5173 --subdomain preview
# share https://preview.neko.computer with a teammate or a webhook
```

### Serve a static site from a sandbox

```bash
neko run --allow-net --tunnel 8000 -- sh -c 'apt-get update && apt-get install -y python3 && cd /workspace && python3 -m http.server 8000'
```

### Reusable runtime, then fast serves

```bash
# once: bake a node runtime into a checkpoint
neko run app --allow-net --checkpoint node -- sh -c 'apt-get update && apt-get install -y nodejs npm'

# many times: instant boot, install deps from the mounted cwd, serve
# node_modules lands in the sandbox overlay, not the user's directory
neko run app --from app@node --tunnel 3000 --allow-host '*.npmjs.org' \
  -- sh -c 'cd /workspace && npm install && npm run dev'
```

### Expose a webhook receiver

```bash
neko run --allow-net --tunnel 4000 -- sh -c 'apt-get update && apt-get install -y python3 && cd /workspace && python3 webhook.py'
# point the provider at https://<sub>.neko.computer
```

## Important constraints

- **Login required.** Every tunnel needs `neko login` (a SuperHQ account). Quotas apply per account (daily and concurrent tunnel limits).
- **`neko run` needs an aarch64 host.** It boots a real microVM (macOS on Apple Silicon, or Linux). `neko tunnel PORT` (local port) works anywhere.
- **No shell by default.** `neko run -- CMD` execs argv directly. Use `-- sh -c '...'` for pipes, `&&`, redirects, or globs.
- **Minimal base image.** The sandbox is a bare Debian aarch64 with no runtime. Install what you need inline (with `--allow-net`), or bake it into a checkpoint.
- **No network by default.** Anything outbound needs `--allow-net` or `--allow-host`. Tunnels, `--port`, and `--term` are unaffected.
- **cwd is mounted read-only at `/workspace`.** The command runs there. Writes still succeed via an overlay but are discarded on exit; pass `--write` to put them on the host, `--mount` to mount somewhere else.
- **Ephemeral unless named or checkpointed.** An anonymous `neko run` discards its disk on exit. Name it, or use `--checkpoint`, to keep state.
- **The tunnel closes when the process exits.** Keep `neko run --tunnel` or `neko tunnel` in the foreground; ctrl-c ends it.

## Deep-dive

- [docs/specs/tunnel-protocol.md](../../docs/specs/tunnel-protocol.md) the tunnel wire protocol
