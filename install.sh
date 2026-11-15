#!/bin/sh
# Install a prebuilt `harbour` binary from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/aryamurray/harbour/main/install.sh | sh
#
# Environment:
#   HARBOUR_VERSION      version to install, e.g. 0.1.0 (default: the latest release)
#   HARBOUR_INSTALL_DIR  where the binary goes (default: ~/.harbour/bin)
#   HARBOUR_BASE_URL     where releases are served from (default: the GitHub repository)
#
# The download is checked against the release's SHA256SUMS before anything is
# installed. Nothing outside HARBOUR_INSTALL_DIR is written, and no shell
# startup file is edited: if the directory is not on PATH, the script prints
# the line to add.

set -eu

# Everything runs from `main`, called on the last line, so a download that is
# cut off part way cannot execute half a script.
main() {
    base_url=${HARBOUR_BASE_URL:-https://github.com/aryamurray/harbour}
    install_dir=${HARBOUR_INSTALL_DIR:-$HOME/.harbour/bin}

    need curl
    need tar
    need uname
    need mktemp

    target=$(detect_target)

    version=${HARBOUR_VERSION:-}
    if [ -z "$version" ]; then
        version=$(latest_version "$base_url")
    fi
    version=${version#v}

    archive="harbour-$version-$target.tar.gz"
    url="$base_url/releases/download/v$version"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    say "downloading harbour $version for $target"
    fetch "$url/$archive" "$tmp/$archive" ||
        die "no release archive at $url/$archive
harbour $version may not exist, or may have no build for $target"
    fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS" ||
        die "could not download $url/SHA256SUMS"

    verify "$tmp" "$archive"

    tar -xzf "$tmp/$archive" -C "$tmp"
    binary="$tmp/harbour-$version-$target/harbour"
    [ -f "$binary" ] || die "the archive does not contain harbour-$version-$target/harbour"

    mkdir -p "$install_dir"
    # Copy beside the destination, then rename: a `harbour` that is running
    # is replaced in one step and never seen half written.
    cp "$binary" "$install_dir/harbour.new"
    chmod 755 "$install_dir/harbour.new"
    mv -f "$install_dir/harbour.new" "$install_dir/harbour"

    say "installed $install_dir/harbour"

    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            say ""
            say "$install_dir is not on your PATH. Add it with:"
            say ""
            say "    export PATH=\"$install_dir:\$PATH\""
            say ""
            say "in your shell's startup file (~/.profile, ~/.bashrc or ~/.zshrc)."
            ;;
    esac
}

say() {
    printf '%s\n' "$1"
}

die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "this installer needs \`$1\`, which was not found"
}

fetch() {
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --output "$2" "$1" 2>/dev/null
}

# The release target for this machine, as the release workflow names it.
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        arm64 | aarch64) arch=aarch64 ;;
        *) die "there is no prebuilt harbour for the $arch architecture; build from source with \`cargo install --path .\`" ;;
    esac

    case "$os" in
        Linux)
            # The Linux binaries are linked against glibc. On musl (Alpine)
            # they would fail at exec with a misleading "not found".
            if ldd --version 2>&1 | grep -qi musl; then
                die "the prebuilt Linux binaries need glibc and this system uses musl; build from source with \`cargo install --path .\`"
            fi
            printf '%s\n' "$arch-unknown-linux-gnu"
            ;;
        Darwin) printf '%s\n' "$arch-apple-darwin" ;;
        *) die "there is no prebuilt harbour for $os here; on Windows download the .zip from the releases page" ;;
    esac
}

# The newest release's version. `releases/latest` redirects to
# `releases/tag/v<version>`; reading the redirect avoids the GitHub API and
# its unauthenticated rate limit.
latest_version() {
    final=$(curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --output /dev/null --write-out '%{url_effective}' "$1/releases/latest" 2>/dev/null) ||
        die "could not reach $1/releases/latest"
    case "$final" in
        */releases/tag/v*) printf '%s\n' "${final##*/releases/tag/v}" ;;
        *) die "$1 has no published release yet; set HARBOUR_VERSION or build from source" ;;
    esac
}

# Check the archive against the release's SHA256SUMS. A missing checksum
# tool is an error, not a skipped check.
verify() {
    expected=$(awk -v f="$2" '$2 == f || $2 == "*" f { print $1 }' "$1/SHA256SUMS")
    [ -n "$expected" ] || die "SHA256SUMS has no entry for $2"

    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$1/$2" | awk '{ print $1 }')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$1/$2" | awk '{ print $1 }')
    else
        die "this installer needs \`sha256sum\` or \`shasum\` to verify the download"
    fi

    [ "$actual" = "$expected" ] || die "checksum mismatch for $2
  expected $expected
  actual   $actual"
}

main "$@"
