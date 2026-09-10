#!/usr/bin/env bash
#
# Configures Cargo to use Codex-built rusty_v8 artifacts on Linux and macOS.
#
# Usage:
#   source ./scripts/env-unix.sh
#   source ./scripts/env-unix.sh --target aarch64-apple-darwin
#   source ./scripts/env-unix.sh --cache-directory /path/to/cache

usage() {
  cat <<'EOF'
Usage: source env-unix.sh [--target TARGET] [--cache-directory DIRECTORY]

Download and verify the Codex-built rusty_v8 archive and generated bindings,
then export RUSTY_V8_ARCHIVE and RUSTY_V8_SRC_BINDING_PATH.
EOF
}

codex_env_unix_error() {
  printf 'error: %s\n' "$*" >&2
  return 1
}

codex_env_unix_download() {
  local uri="$1"
  local output_path="$2"

  if ! curl \
    --fail \
    --location \
    --silent \
    --show-error \
    --retry 3 \
    --retry-all-errors \
    --connect-timeout 30 \
    --output "$output_path" \
    "$uri"; then
    codex_env_unix_error "Failed to download $uri with curl."
    return 1
  fi
}

codex_env_unix_sha256() {
  local file_path="$1"
  local hash_output
  local hash

  if command -v sha256sum >/dev/null 2>&1; then
    if ! hash_output="$(sha256sum "$file_path")"; then
      return 1
    fi
  elif command -v shasum >/dev/null 2>&1; then
    if ! hash_output="$(shasum -a 256 "$file_path")"; then
      return 1
    fi
  else
    return 1
  fi

  hash="${hash_output%%[[:space:]]*}"
  printf '%s\n' "$hash" | tr '[:upper:]' '[:lower:]'
}

codex_env_unix_manifest_hash() {
  local manifest_path="$1"
  local asset_name="$2"
  local hash

  if ! hash="$(awk -v expected="$asset_name" '
        {
            sub(/\r$/, "", $0)
            if ($2 == expected || $2 == "*" expected) {
                print tolower($1)
            }
        }
    ' "$manifest_path")"; then
    return 1
  fi

  if [[ "${#hash}" -ne 64 || ! "$hash" =~ ^[0-9a-f]{64}$ ]]; then
    return 1
  fi

  printf '%s\n' "$hash"
}

codex_env_unix_main() {
  local requested_target=''
  local requested_cache_directory=''
  local platform
  local cargo_toml_path
  local version_matches
  local version_match_count
  local version
  local rustc_version
  local host_matches
  local host_match_count
  local cache_root
  local artifact_directory
  local profile='ptrcomp_sandbox_release'
  local release_tag
  local base_url
  local archive_name
  local binding_name
  local checksums_name
  local checksums_path
  local checksums_temporary_path
  local asset_name
  local asset_path
  local expected_hash
  local cached_hash
  local temporary_path
  local actual_hash

  while [[ "$#" -gt 0 ]]; do
    case "$1" in
      --help | -h)
        usage
        return 0
        ;;
      --target | -t)
        if [[ "$#" -lt 2 || -z "$2" || "$2" == -* ]]; then
          codex_env_unix_error "--target requires a target triple."
          return 1
        fi
        requested_target="$2"
        shift 2
        ;;
      --cache-directory | -c)
        if [[ "$#" -lt 2 || -z "$2" ]]; then
          codex_env_unix_error "--cache-directory requires a directory."
          return 1
        fi
        requested_cache_directory="$2"
        shift 2
        ;;
      *)
        codex_env_unix_error "Unknown argument: $1"
        usage >&2
        return 1
        ;;
    esac
  done

  if ! platform="$(uname -s)"; then
    codex_env_unix_error 'Failed to determine the host operating system with uname.'
    return 1
  fi
  case "$platform" in
    Linux | Darwin)
      ;;
    *)
      codex_env_unix_error "Unsupported host operating system: $platform"
      return 1
      ;;
  esac

  if ! command -v curl >/dev/null 2>&1; then
    codex_env_unix_error 'curl is required but was not found on PATH.'
    return 1
  fi
  if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    codex_env_unix_error 'sha256sum or shasum is required but neither was found on PATH.'
    return 1
  fi

  cargo_toml_path="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/Cargo.toml"
  if [[ ! -f "$cargo_toml_path" ]]; then
    codex_env_unix_error "Cargo manifest not found: $cargo_toml_path"
    return 1
  fi

  if ! version_matches="$(sed -nE 's/^v8[[:space:]]*=[[:space:]]*"=([0-9]+\.[0-9]+\.[0-9]+)"[[:space:]]*$/\1/p' "$cargo_toml_path")"; then
    codex_env_unix_error "Failed to read the pinned v8 version from $cargo_toml_path"
    return 1
  fi
  if ! version_match_count="$(printf '%s\n' "$version_matches" | awk 'NF { count += 1 } END { print count + 0 }')"; then
    codex_env_unix_error "Failed to count pinned v8 versions in $cargo_toml_path"
    return 1
  fi
  if [[ "$version_match_count" -ne 1 ]]; then
    codex_env_unix_error "Expected exactly one pinned v8 version in $cargo_toml_path"
    return 1
  fi
  version="$version_matches"

  if [[ -z "$requested_target" ]]; then
    if ! rustc_version="$(rustc -vV 2>/dev/null)"; then
      codex_env_unix_error 'Failed to determine the host target with rustc.'
      return 1
    fi
    if ! host_matches="$(printf '%s\n' "$rustc_version" | sed -n 's/^host:[[:space:]]*//p')"; then
      codex_env_unix_error 'Failed to parse the host target from rustc -vV.'
      return 1
    fi
    if ! host_match_count="$(printf '%s\n' "$host_matches" | awk 'NF { count += 1 } END { print count + 0 }')"; then
      codex_env_unix_error 'Failed to count host targets reported by rustc.'
      return 1
    fi
    if [[ "$host_match_count" -ne 1 ]]; then
      codex_env_unix_error 'Unable to determine the host target from rustc -vV.'
      return 1
    fi
    requested_target="$host_matches"
  fi

  case "$requested_target" in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu | x86_64-unknown-linux-musl | aarch64-unknown-linux-musl | x86_64-apple-darwin | aarch64-apple-darwin)
      ;;
    *)
      codex_env_unix_error "Unsupported Unix target: $requested_target"
      return 1
      ;;
  esac

  if [[ -z "$requested_cache_directory" ]]; then
    if [[ -z "${HOME:-}" ]]; then
      codex_env_unix_error 'HOME is not set, so the default cache directory cannot be determined.'
      return 1
    fi
    if [[ -n "${XDG_CACHE_HOME:-}" ]]; then
      cache_root="$XDG_CACHE_HOME"
    elif [[ "$platform" == 'Darwin' ]]; then
      cache_root="$HOME/Library/Caches"
    else
      cache_root="$HOME/.cache"
    fi
    requested_cache_directory="$cache_root/codex/rusty-v8"
  fi

  release_tag="rusty-v8-v$version"
  base_url="https://github.com/openai/codex/releases/download/$release_tag"
  artifact_directory="$requested_cache_directory/$version/$requested_target"
  archive_name="librusty_v8_${profile}_${requested_target}.a.gz"
  binding_name="src_binding_${profile}_${requested_target}.rs"
  checksums_name="rusty_v8_${profile}_${requested_target}.sha256"
  checksums_path="$artifact_directory/$checksums_name"

  if ! mkdir -p "$artifact_directory"; then
    codex_env_unix_error "Failed to create artifact directory: $artifact_directory"
    return 1
  fi

  if ! checksums_temporary_path="$(mktemp "$checksums_path.XXXXXX")"; then
    codex_env_unix_error "Failed to create a temporary checksum manifest in $artifact_directory"
    return 1
  fi
  if ! codex_env_unix_download "$base_url/$checksums_name" "$checksums_temporary_path"; then
    rm -f "$checksums_temporary_path"
    return 1
  fi
  if ! mv -f "$checksums_temporary_path" "$checksums_path"; then
    rm -f "$checksums_temporary_path"
    codex_env_unix_error "Failed to store checksum manifest: $checksums_path"
    return 1
  fi

  if ! version_match_count="$(awk 'NF { count += 1 } END { print count + 0 }' "$checksums_path")"; then
    codex_env_unix_error "Failed to read checksum manifest: $checksums_path"
    return 1
  fi
  if [[ "$version_match_count" -ne 2 ]]; then
    codex_env_unix_error "Expected exactly two checksums in $checksums_path"
    return 1
  fi

  for asset_name in "$archive_name" "$binding_name"; do
    if ! expected_hash="$(codex_env_unix_manifest_hash "$checksums_path" "$asset_name")"; then
      codex_env_unix_error "Missing or invalid checksum for $asset_name in $checksums_path"
      return 1
    fi

    asset_path="$artifact_directory/$asset_name"
    cached_hash=''
    if [[ -f "$asset_path" ]] && cached_hash="$(codex_env_unix_sha256 "$asset_path")" && [[ "$cached_hash" == "$expected_hash" ]]; then
      continue
    fi

    if ! temporary_path="$(mktemp "$asset_path.XXXXXX")"; then
      codex_env_unix_error "Failed to create a temporary file for $asset_name"
      return 1
    fi
    printf 'Downloading %s\n' "$asset_name"
    if ! codex_env_unix_download "$base_url/$asset_name" "$temporary_path"; then
      rm -f "$temporary_path"
      return 1
    fi
    if ! actual_hash="$(codex_env_unix_sha256 "$temporary_path")"; then
      rm -f "$temporary_path"
      codex_env_unix_error "Failed to calculate the SHA-256 checksum for $asset_name"
      return 1
    fi
    if [[ "$actual_hash" != "$expected_hash" ]]; then
      rm -f "$temporary_path"
      codex_env_unix_error "Checksum mismatch for $asset_name (expected $expected_hash, got $actual_hash)"
      return 1
    fi
    if ! mv -f "$temporary_path" "$asset_path"; then
      rm -f "$temporary_path"
      codex_env_unix_error "Failed to store downloaded asset: $asset_path"
      return 1
    fi
  done

  export RUSTY_V8_ARCHIVE="$artifact_directory/$archive_name"
  export RUSTY_V8_SRC_BINDING_PATH="$artifact_directory/$binding_name"

  printf 'RUSTY_V8_ARCHIVE=%s\n' "$RUSTY_V8_ARCHIVE"
  printf 'RUSTY_V8_SRC_BINDING_PATH=%s\n' "$RUSTY_V8_SRC_BINDING_PATH"
}

codex_env_unix_main "$@"
