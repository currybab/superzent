#!/usr/bin/env sh
set -eu

bundle_basename() {
    case "$channel" in
        stable)
            echo "superzent"
            ;;
        dev)
            echo "superzent-dev"
            ;;
        *)
            echo "Unsupported release channel: $channel" >&2
            exit 1
            ;;
    esac
}

linux_app_id() {
    case "$channel" in
        stable)
            echo "ai.nangman.superzent"
            ;;
        dev)
            echo "ai.nangman.superzent-dev"
            ;;
        *)
            echo "Unsupported release channel: $channel" >&2
            exit 1
            ;;
    esac
}

download() {
    if command -v curl >/dev/null 2>&1; then
        command curl -fsSL "$@"
    elif command -v wget >/dev/null 2>&1; then
        wget -O- "$@"
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi
}

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${SUPERZENT_CHANNEL:-${ZED_CHANNEL:-stable}}"
    releases_base_url="${SUPERZENT_RELEASES_URL:-https://releases.nangman.ai}"
    releases_base_url="${releases_base_url%/}"
    bundle_path="${SUPERZENT_BUNDLE_PATH:-${ZED_BUNDLE_PATH:-}}"

    case "$platform" in
        Darwin)
            platform="macos"
            ;;
        Linux)
            platform="linux"
            ;;
        *)
            echo "Unsupported platform $platform"
            exit 1
            ;;
    esac

    case "$platform-$arch" in
        macos-arm64*)
            arch="aarch64"
            ;;
        macos-x86*)
            arch="x86_64"
            ;;
        linux-arm64* | linux-armhf | linux-aarch64)
            arch="aarch64"
            ;;
        linux-x86* | linux-i686*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp_dir="$(mktemp -d "$TMPDIR/superzent-XXXXXX")"
    else
        temp_dir="$(mktemp -d "/tmp/superzent-XXXXXX")"
    fi

    "$platform"

    if [ "$(command -v superzent 2>/dev/null || true)" = "$HOME/.local/bin/superzent" ]; then
        echo "superzent has been installed. Run with 'superzent'"
    else
        echo "To run superzent from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run superzent now, '~/.local/bin/superzent'"
    fi
}

linux() {
    archive_path="$temp_dir/$(bundle_basename)-linux-$arch.tar.gz"
    bundle_dir="$HOME/.local/$(bundle_basename).app"
    app_id="$(linux_app_id)"

    if [ -n "$bundle_path" ]; then
        cp "$bundle_path" "$archive_path"
    else
        echo "Published Linux release bundles are not available. Build locally with ./script/install-linux."
        exit 1
    fi

    rm -rf "$bundle_dir"
    mkdir -p "$bundle_dir"
    tar -xzf "$archive_path" -C "$HOME/.local/"

    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
    ln -sf "$bundle_dir/bin/superzent" "$HOME/.local/bin/superzent"

    desktop_file_path="$HOME/.local/share/applications/${app_id}.desktop"
    src_dir="$bundle_dir/share/applications"
    if [ -f "$src_dir/${app_id}.desktop" ]; then
        cp "$src_dir/${app_id}.desktop" "${desktop_file_path}"
    else
        echo "Missing desktop file for ${app_id}" >&2
        exit 1
    fi
    sed -i "s|Icon=superzent|Icon=$bundle_dir/share/icons/hicolor/512x512/apps/superzent.png|g" "${desktop_file_path}"
    sed -i "s|Exec=superzent|Exec=$bundle_dir/bin/superzent|g" "${desktop_file_path}"
}

macos() {
    dmg_path="$temp_dir/$(bundle_basename)-$arch.dmg"

    if [ -n "$bundle_path" ]; then
        cp "$bundle_path" "$dmg_path"
    else
        if [ "$channel" != "stable" ]; then
            echo "Published downloads are only available for the stable channel." >&2
            exit 1
        fi
        if [ "$arch" != "aarch64" ]; then
            echo "Published macOS downloads are only available for Apple Silicon." >&2
            exit 1
        fi

        echo "Downloading superzent version: latest"
        download "${releases_base_url}/releases/${channel}/latest/download?asset=superzent&os=macos&arch=${arch}&source=install.sh" > "$dmg_path"
    fi

    hdiutil attach -quiet "$dmg_path" -mountpoint "$temp_dir/mount"
    app="$(cd "$temp_dir/mount/" && echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp_dir/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp_dir/mount"

    mkdir -p "$HOME/.local/bin"
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/superzent"
}

main "$@"
