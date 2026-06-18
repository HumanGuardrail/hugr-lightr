# Building Lightr

Three independent artifacts: the **host binary** (`lightr`), the **guest init**
(`lightr-init`), and the **kernel image**. Only the kernel needs a Linux build
environment; the rest compile on macOS with standard tooling.

---

## 1. Host binary (`lightr`)

The `lightr` CLI and engine run on the host (macOS or Linux).

```sh
# Plain build (all engines):
cargo build --release -p lightr-cli

# macOS with Apple Virtualization.framework (vz engine):
cargo build --release -p lightr-cli --features vz
```

On macOS the `vz` binary needs an entitlement to use the Hypervisor framework:

```sh
# Ad-hoc codesign (development; sufficient for local use):
codesign --entitlements packaging/macos/lightr.entitlements \
         -s - target/release/lightr
```

The entitlements file is at `packaging/macos/lightr.entitlements`. A proper
Developer ID signature is required for distribution outside development.

---

## 2. Guest init binary (`lightr-init`) — docker-free

`lightr-init` is the guest PID 1 that runs inside the microVM. It must be a
**static Linux musl ELF**. On macOS it cross-compiles with no docker via
`cargo-zigbuild` (zig as the musl cross-linker).

```sh
# Build for the default host-mapped arch (aarch64 on Apple Silicon, x86_64 on Intel):
scripts/build-init.sh

# Explicit arch:
scripts/build-init.sh --arch aarch64
scripts/build-init.sh --arch x86_64

# Custom output path:
scripts/build-init.sh --arch aarch64 --out build/lightr-init-arm64
```

The script checks for `zig`, `cargo-zigbuild`, and the rustup musl target,
and prints the exact install command for anything missing. No container needed.

**One-time setup** (if not already present):

```sh
brew install zig
cargo install cargo-zigbuild
rustup target add aarch64-unknown-linux-musl   # for Apple Silicon guests
rustup target add x86_64-unknown-linux-musl    # for Intel guests
```

---

## 3. Kernel image

The Linux kernel cannot cross-compile natively on macOS; a Linux build
environment is required. Two scripts handle this via Docker. For arm64 there
is a no-docker alternative.

### x86_64 kernel (bzImage)

```sh
scripts/build-kernel-x86.sh [--out build/linux-pack-x86]
```

Requires Docker (runs a `linux/amd64` Debian bookworm container). Produces
`build/linux-pack-x86/bzImage`.

### arm64 kernel (Image)

```sh
scripts/build-kernel-arm64.sh [--out build/linux-pack-arm64]
```

Requires Docker (cross-compiles via `aarch64-linux-gnu` toolchain inside a
`linux/amd64` container). Produces `build/linux-pack-arm64/Image`.

**No-docker alternatives for arm64:**

- Apple's [Containerization framework](https://github.com/apple/containerization)
  ships a prebuilt arm64 VZ kernel. On Apple Silicon this is the fastest path:
  copy the prebuilt `Image` directly into your pack without running the kernel
  build script.
- Build the kernel on a Linux arm64 target machine and copy out
  `arch/arm64/boot/Image`.

---

## 4. Assembling and installing a pack

A pack bundles a kernel image and the `lightr-init` binary into the directory
structure the vz engine loads at boot.

```sh
# Example: arm64 pack
cargo run -p lightr-engine --example assemble-pack -- \
    --kernel build/linux-pack-arm64/Image \
    --init   build/lightr-init-arm64 \
    --out    build/vz-pack-arm64 \
    --arch   aarch64 \
    --kernel-version 6.18.5

# Example: x86_64 pack
cargo run -p lightr-engine --example assemble-pack -- \
    --kernel build/linux-pack-x86/bzImage \
    --init   target/x86_64-unknown-linux-musl/release/lightr-init \
    --out    build/vz-pack-x86 \
    --arch   x86_64 \
    --kernel-version 6.18.5
```

The assembled pack directory can be passed to `lightr run --pack <dir>` or
placed in the default pack search path. See `docs/ARCHITECTURE.md` for the
pack format and engine boot contract.
