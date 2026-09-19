#!/usr/bin/env bash
#
# Builds the Unix binaries shipped by the Codex release workflow.
#
# Usage:
#   bash ./scripts/build-unix.sh
#   bash ./scripts/build-unix.sh --bundle primary
#   bash ./scripts/build-unix.sh --target aarch64-apple-darwin
#   bash ./scripts/build-unix.sh --target x86_64-unknown-linux-musl

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: build-unix.sh [OPTIONS]

Build the Linux or macOS release binaries defined in
.github/workflows/rust-release.yml.

Options:
  -t, --target TARGET              Rust target triple. Defaults to the host.
  -b, --bundle BUNDLE              all, primary, or app-server. Default: all.
      --v8-cache-directory DIR     Cache used by env-unix.sh for rusty_v8.
  -h, --help                       Show this help.

Release targets:
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
  x86_64-apple-darwin
  aarch64-apple-darwin

Native development targets x86_64-unknown-linux-gnu and
aarch64-unknown-linux-gnu are also supported.

The Rust target and native build tools must already be installed. In
particular, Linux primary builds require pkg-config, libcap headers, strip,
and sha256sum. Musl builds require the equivalent musl toolchain used by
.github/scripts/install-musl-build-tools.sh.
EOF
}

error() {
  printf 'error: %s\n' "$*" >&2
}

target=''
bundle='all'
v8_cache_directory=''
uses_explicit_target='false'

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    -t | --target)
      if [[ "$#" -lt 2 || -z "$2" || "$2" == -* ]]; then
        error '--target requires a target triple.'
        exit 1
      fi
      target="$2"
      uses_explicit_target='true'
      shift 2
      ;;
    -b | --bundle)
      if [[ "$#" -lt 2 || -z "$2" || "$2" == -* ]]; then
        error '--bundle requires all, primary, or app-server.'
        exit 1
      fi
      bundle="$2"
      shift 2
      ;;
    --v8-cache-directory | --cache-directory)
      if [[ "$#" -lt 2 || -z "$2" ]]; then
        error '--v8-cache-directory requires a directory.'
        exit 1
      fi
      v8_cache_directory="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      error "Unknown argument: $1"
      usage >&2
      exit 1
      ;;
  esac
done

case "$bundle" in
  all | primary | app-server)
    ;;
  *)
    error "Unsupported bundle: $bundle"
    exit 1
    ;;
esac

for command_name in cargo git rustc uname; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    error "$command_name is required but was not found on PATH."
    exit 1
  fi
done

platform="$(uname -s)"
case "$platform" in
  Linux | Darwin)
    ;;
  *)
    error "Unsupported host operating system: $platform"
    exit 1
    ;;
esac

if [[ -z "$target" ]]; then
  rustc_version="$(rustc -vV)"
  target="$(printf '%s\n' "$rustc_version" | sed -n 's/^host:[[:space:]]*//p')"
  target_count="$(printf '%s\n' "$target" | awk 'NF { count += 1 } END { print count + 0 }')"
  if [[ "$target_count" -ne 1 ]]; then
    error 'Unable to determine the host target from rustc -vV.'
    exit 1
  fi
fi

case "$target" in
  x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu | x86_64-unknown-linux-musl | aarch64-unknown-linux-musl | x86_64-apple-darwin | aarch64-apple-darwin)
    ;;
  *)
    error "Unsupported Unix target: $target"
    exit 1
    ;;
esac

case "$platform:$target" in
  Linux:*linux* | Darwin:*apple-darwin)
    ;;
  *)
    error "Target $target cannot be built on $platform with this script."
    exit 1
    ;;
esac

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
codex_rs_root="$(cd "$script_dir/.." && pwd)"
repository_root="$(cd "$codex_rs_root/.." && pwd)"

export CODEX_REPO_ROOT="$repository_root"
v8_environment_args=(--target "$target")
if [[ -n "$v8_cache_directory" ]]; then
  v8_environment_args+=(--cache-directory "$v8_cache_directory")
fi
# shellcheck source=env-unix.sh
if ! source "$script_dir/env-unix.sh" "${v8_environment_args[@]}"; then
  error 'Failed to configure rusty_v8 artifacts.'
  exit 1
fi

export CARGO_NET_GIT_FETCH_WITH_CLI='true'
case "$target" in
  *apple-darwin)
    export CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO='packed'
    ;;
  *linux*)
    export CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO='off'
    ;;
esac
STABLE_GIT_COMMIT="$(git -C "$repository_root" rev-parse HEAD)"
export STABLE_GIT_COMMIT

primary_binaries=(
  codex
  codex-code-mode-host
)
app_server_binaries=(
  codex-code-mode-host
)
case "$bundle" in
  all)
    binaries=(
      codex
      codex-code-mode-host
    )
    ;;
  primary)
    binaries=("${primary_binaries[@]}")
    ;;
  app-server)
    binaries=("${app_server_binaries[@]}")
    ;;
esac

build_bwrap='false'
if [[ "$target" == *linux* && "$bundle" != 'app-server' ]]; then
  build_bwrap='true'
fi

cargo_target_root="${CARGO_TARGET_DIR:-$codex_rs_root/target}"
if [[ "$cargo_target_root" != /* ]]; then
  cargo_target_root="$codex_rs_root/$cargo_target_root"
fi
if [[ "$uses_explicit_target" == 'true' ]]; then
  release_directory="$cargo_target_root/$target/release"
else
  release_directory="$cargo_target_root/release"
fi

cargo_arguments=(build --release --timings)
if [[ "$uses_explicit_target" == 'true' ]]; then
  cargo_arguments+=(--target "$target")
fi

display_binaries=("${binaries[@]}")
if [[ "$build_bwrap" == 'true' ]]; then
  display_binaries+=(bwrap)
fi

printf 'Building Unix %s bundle for %s\n' "$bundle" "$target"
printf 'Binaries: %s\n' "$(IFS=', '; echo "${display_binaries[*]}")"

pushd "$codex_rs_root" >/dev/null
if [[ "$build_bwrap" == 'true' ]]; then
  for command_name in pkg-config strip sha256sum; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
      error "$command_name is required for the Linux primary bundle."
      exit 1
    fi
  done
  if ! pkg-config --exists libcap; then
    error 'libcap development files are required for the Linux primary bundle.'
    exit 1
  fi

  cargo "${cargo_arguments[@]}" --bin bwrap
  bwrap_path="$release_directory/bwrap"
  if [[ ! -f "$bwrap_path" ]]; then
    error "bwrap binary not found: $bwrap_path"
    exit 1
  fi
  strip --strip-debug --strip-unneeded "$bwrap_path"
  CODEX_BWRAP_SHA256="$(sha256sum "$bwrap_path" | awk '{print $1}')"
  export CODEX_BWRAP_SHA256
  printf 'Built bwrap with sha256:%s\n' "$CODEX_BWRAP_SHA256"
fi

for binary in "${binaries[@]}"; do
  cargo_arguments+=(--bin "$binary")
done
cargo "${cargo_arguments[@]}"
popd >/dev/null

printf 'Build complete: %s\n' "$release_directory"
