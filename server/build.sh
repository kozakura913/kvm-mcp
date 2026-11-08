#!/usr/bin/env bash
# Local cross-compile for the Raspberry Pi Zero (armv6l) using Docker on this
# machine. Produces a fully static musl binary (no glibc dependency on the Pi).
#
# Usage:
#   ./build.sh            # release build for arm-unknown-linux-musleabi
#   ./build.sh test       # run unit tests natively (x86_64) inside the container
#   ./build.sh check      # cargo check natively
#
# Note: -Z build-std is required because arm-unknown-linux-musleabi is a tier-3
# target (no precompiled std). RUSTC_BOOTSTRAP=1 keeps the stable toolchain usable.
set -euo pipefail
cd "$(dirname "$0")"

TARGET=arm-unknown-linux-musleabi
TC=vendor/arm-linux-musleabi-cross
IMAGE=${BUILD_IMAGE:-rust:1-slim-trixie}

# unpack the musl cross toolchain once (on the host: container-side tar cannot
# chown on bind mounts)
if [ ! -x "$TC/bin/arm-linux-musleabi-gcc" ]; then
  [ -f vendor/arm-linux-musleabi-cross.tgz ] || {
    echo "missing vendor/arm-linux-musleabi-cross.tgz" >&2
    echo "download: curl -o vendor/arm-linux-musleabi-cross.tgz https://musl.cc/arm-linux-musleabi-cross.tgz" >&2
    exit 1
  }
  tar xzf vendor/arm-linux-musleabi-cross.tgz -C vendor/ --no-same-owner
fi

mkdir -p .cargo-cache
# extra docker args can be supplied via BUILD_DOCKER_ARGS (e.g. CA bundle for proxied networks)
EXTRA_ARGS=()
[ -n "${BUILD_DOCKER_ARGS:-}" ] && read -r -a EXTRA_ARGS <<<"$BUILD_DOCKER_ARGS"

docker run --rm \
  --network=host \
  -v "$PWD":/work -w /work \
  -v "$PWD/.cargo-cache":/usr/local/cargo/registry \
  -e CARGO_HOME=/usr/local/cargo \
  "${EXTRA_ARGS[@]}" \
  "$IMAGE" bash -euo pipefail -c '
    export PATH=/work/vendor/arm-linux-musleabi-cross/bin:$PATH
    export RUSTC_BOOTSTRAP=1
    rustup component add rust-src >/dev/null

    # tier-3 build-std targets ship no crt objects or unwinder: stage them
    # into the self-contained dirs inside the rustc sysroot (where rustc looks)
    TC=/work/vendor/arm-linux-musleabi-cross
    GCCD=$(ls -d $TC/lib/gcc/arm-linux-musleabi/*/ | head -1)
    RD=$(rustc --print sysroot)/lib/rustlib/arm-unknown-linux-musleabi/lib
    mkdir -p $RD/self-contained
    cp -f $TC/arm-linux-musleabi/lib/crt1.o $TC/arm-linux-musleabi/lib/crti.o $TC/arm-linux-musleabi/lib/crtn.o $RD/self-contained/
    cp -f $GCCD/crtbegin.o $GCCD/crtend.o $GCCD/crtbeginT.o $GCCD/crtendS.o $RD/self-contained/
    cp -f $GCCD/libgcc.a $GCCD/libgcc_eh.a $RD/
    cp -f $GCCD/libgcc_eh.a $RD/libunwind.a

    case "${1:-build}" in
      build)
        cargo build --release --target arm-unknown-linux-musleabi -Zbuild-std=std,panic_abort
        ls -l target/arm-unknown-linux-musleabi/release/usb-mcp
        ;;
      test)
        cargo test --workspace
        ;;
      check)
        cargo check
        ;;
    esac
  ' _ "${1:-build}"
