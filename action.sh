#!/usr/bin/env bash
# Installs a kilit release and puts kilit on the PATH; action.yml runs it.
#
# KILIT_VERSION is the release, such as 0.1.0. The runner sets RUNNER_OS,
# RUNNER_ARCH, RUNNER_TEMP and GITHUB_PATH. KILIT_INSTALL_ARCHIVES, which
# only the release workflow's own test sets, names a folder that holds the
# archives and SHA256SUMS to take instead of the release's.
#
# Written for bash 3.2, the bash of macOS.

set -euo pipefail

repository=https://github.com/Veysel440/kilit

fail() {
  echo "::error::$1"
  exit 1
}

# The input is never printed: a newline in it could start a workflow command.
version=${KILIT_VERSION-}
case $version in
  '' | *[!0123456789.]* | .* | *. | *..*)
    fail "The version input must be a kilit release such as 0.1.0: digits and dots only."
    ;;
esac
if [ "${version//[0123456789]/}" != .. ]; then
  fail "The version input must be a kilit release such as 0.1.0: three numbers and two dots."
fi

os=${RUNNER_OS-}
arch=${RUNNER_ARCH-}
case "$os/$arch" in
  Linux/X64) target=x86_64-unknown-linux-musl ;;
  Linux/ARM64) target=aarch64-unknown-linux-musl ;;
  macOS/X64) target=x86_64-apple-darwin ;;
  macOS/ARM64) target=aarch64-apple-darwin ;;
  Windows/X64) target=x86_64-pc-windows-msvc ;;
  *)
    fail "kilit has no prebuilt binary for RUNNER_OS=${os:-(not set)} and RUNNER_ARCH=${arch:-(not set)}. Build it from source instead: $repository#installation"
    ;;
esac

if [ -z "${RUNNER_TEMP-}" ] || [ -z "${GITHUB_PATH-}" ]; then
  fail "RUNNER_TEMP or GITHUB_PATH is not set: run this through action.yml in a GitHub Actions job."
fi

# Git Bash on Windows gets Windows paths from the runner; its own tools take
# either form, Windows programs and GITHUB_PATH need the Windows one.
unix_path() {
  if [ "$os" = Windows ]; then cygpath -u "$1"; else printf '%s\n' "$1"; fi
}

if [ "$os" = Windows ]; then
  archive=kilit-$version-$target.zip
  program=kilit.exe
else
  archive=kilit-$version-$target.tar.gz
  program=kilit
fi
name=kilit-$version-$target
dir=$(mktemp -d "$(unix_path "$RUNNER_TEMP")/kilit.XXXXXX")

if [ -n "${KILIT_INSTALL_ARCHIVES-}" ]; then
  folder=$(unix_path "$KILIT_INSTALL_ARCHIVES")
  echo "Taking the archives from $folder (KILIT_INSTALL_ARCHIVES), not from the release."
  fetch() {
    if [ ! -f "$folder/$1" ]; then
      fail "$folder has no $1."
    fi
    cp "$folder/$1" "$dir/$1"
  }
else
  if ! command -v curl > /dev/null 2>&1; then
    fail "curl is not installed, so kilit $version cannot be downloaded."
  fi
  base=$repository/releases/download/v$version
  fetch() {
    local status
    if status=$(curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
      --fail --location --silent --show-error --retry 5 --retry-delay 2 \
      --output "$dir/$1" --write-out '%{http_code}' "$base/$1"); then
      return 0
    fi
    if [ "$status" = 404 ]; then
      fail "kilit $version has no release file $1 ($base/$1 gives 404): is there a release v$version?"
    fi
    fail "Could not download $base/$1 (HTTP status $status)."
  }
fi

fetch SHA256SUMS
fetch "$archive"

expected=$(awk -v file="$archive" '$2 == file || $2 == "*" file { print $1 }' "$dir/SHA256SUMS")
case $expected in
  '' | *[!0123456789abcdef]*)
    fail "SHA256SUMS of kilit $version has no single valid line for $archive."
    ;;
esac
if [ "${#expected}" -ne 64 ]; then
  fail "SHA256SUMS of kilit $version has no single valid line for $archive."
fi

if command -v sha256sum > /dev/null 2>&1; then
  actual=$(sha256sum -b < "$dir/$archive")
elif command -v shasum > /dev/null 2>&1; then
  actual=$(shasum -a 256 -b < "$dir/$archive")
else
  fail "Neither sha256sum nor shasum is installed, so $archive cannot be checked."
fi
actual=${actual%% *}
if [ "$actual" != "$expected" ]; then
  fail "The checksum of $archive does not match SHA256SUMS (expected $expected, got $actual); nothing was unpacked."
fi

if [ "$os" = Windows ]; then
  # Git Bash's tar cannot read a zip file; the tar of Windows itself can.
  "$(cygpath -u -S)/tar.exe" -x \
    -f "$(cygpath -w "$dir/$archive")" -C "$(cygpath -w "$dir")"
else
  tar -x -z -f "$dir/$archive" -C "$dir"
fi

if [ ! -f "$dir/$name/$program" ]; then
  fail "$archive has no $name/$program."
fi

if [ "$os" = Windows ]; then
  bin=$(cygpath -w "$dir/$name")
else
  bin=$dir/$name
fi
echo "$bin" >> "$GITHUB_PATH"
echo "kilit $version ($target) is installed in $bin."
