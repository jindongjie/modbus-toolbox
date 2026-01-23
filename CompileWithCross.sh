#!/bin/bash


# 兼容性目标架构数组
compatibility_targets=(
    # "aarch64-linux-android"
    # "aarch64-unknown-linux-gnu"
    # "aarch64-unknown-linux-musl"
    # "arm-linux-androideabi"
    # "arm-unknown-linux-gnueabi"
    # "arm-unknown-linux-gnueabihf"
    # "arm-unknown-linux-musleabi"
    # "arm-unknown-linux-musleabihf"
    # "armv5te-unknown-linux-gnueabi"
    # "armv5te-unknown-linux-musleabi"
    # "armv7-linux-androideabi"
    # "armv7-unknown-linux-gnueabi"
    # "armv7-unknown-linux-gnueabihf"
    # "armv7-unknown-linux-musleabi"
    # "armv7-unknown-linux-musleabihf"
    # "i586-unknown-linux-gnu"
    # "i586-unknown-linux-musl"
    # "i686-unknown-freebsd"
    # "i686-linux-android"
    "i686-pc-windows-gnu"
    "i686-unknown-linux-gnu"
    # "i686-unknown-linux-musl"
    "mips-unknown-linux-gnu"
    # "mips-unknown-linux-musl"
    # "mips64-unknown-linux-gnuabi64"
    # "mips64-unknown-linux-muslabi64"
    # "mips64el-unknown-linux-gnuabi64"
    # "mips64el-unknown-linux-muslabi64"
    # "mipsel-unknown-linux-gnu"
    # "mipsel-unknown-linux-musl"
    # "powerpc-unknown-linux-gnu"
    # "powerpc64-unknown-linux-gnu"
    # "powerpc64le-unknown-linux-gnu"
    # "riscv64gc-unknown-linux-gnu"
    # "s390x-unknown-linux-gnu"
    # "sparc64-unknown-linux-gnu"
    # "sparcv9-sun-solaris"
    # "thumbv6m-none-eabi"
    # "thumbv7em-none-eabi"
    # "thumbv7em-none-eabihf"
    # "thumbv7m-none-eabi"
    # "thumbv7neon-linux-androideabi"
    # "thumbv7neon-unknown-linux-gnueabihf"
    # "wasm32-unknown-emscripten"
    # "x86_64-linux-android"
    "x86_64-pc-windows-gnu"
    # "x86_64-sun-solaris"
    # "x86_64-unknown-freebsd"
    # "x86_64-unknown-dragonfly"
    # "x86_64-unknown-illumos"
    "x86_64-unknown-linux-gnu"
    # "x86_64-unknown-linux-gnu:centos"
    # "x86_64-unknown-linux-musl"
    # "x86_64-unknown-netbsd"
)

echo "# Cross-compilation build commands for all mainstream targets"
echo ""

# Generate build commands for each target
for target in "${compatibility_targets[@]}"; do
    echo "# Build for $target"
    cross build --target $target --release
    echo ""
done

# Generate test commands for each target
echo "# Test commands for all mainstream targets"
for target in "${compatibility_targets[@]}"; do
    echo "# Test for $target"
    cross test --target $target
    echo ""
done

# Generate release build commands with optimization flags
# echo "# Release builds with LTO optimization for all mainstream targets"
# for target in "${compatibility_targets[@]}"; do
#     echo "# Release build with LTO for $target"
#     echo "cross rustc --target $target --release -- -C lto"
#     echo ""
# done
