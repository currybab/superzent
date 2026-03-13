#!/usr/bin/env sh
set -eu

app_dir_name() {
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

mac_app_bundle() {
    case "$channel" in
        stable)
            echo "superzent.app"
            ;;
        dev)
            echo "superzent dev.app"
            ;;
        *)
            echo "Unsupported release channel: $channel" >&2
            exit 1
            ;;
    esac
}

mac_bundle_id() {
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

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        remaining=$(ls -d /Applications/superzent*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        remaining=$(ls -d "$HOME/.local/superzent"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    config_dir="$1"
    if [ ! -d "$config_dir" ]; then
        return
    fi

    printf "Do you want to keep your superzent preferences for %s? [Y/n] " "$channel"
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$config_dir"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${SUPERZENT_CHANNEL:-${ZED_CHANNEL:-stable}}"

    case "$platform" in
        Darwin)
            macos
            ;;
        Linux)
            linux
            ;;
        *)
            echo "Unsupported platform $platform"
            exit 1
            ;;
    esac

    echo "superzent has been uninstalled"
}

linux() {
    name="$(app_dir_name)"
    app_id="$(linux_app_id)"
    config_dir="$HOME/.config/$name"
    data_dir="$HOME/.local/share/$name"
    state_dir="$HOME/.local/state/$name"
    cache_dir="$HOME/.cache/$name"

    rm -rf "$HOME/.local/$name.app"
    rm -f "$HOME/.local/bin/superzent"
    rm -f "$HOME/.local/share/applications/${app_id}.desktop"
    rm -rf "$data_dir" "$state_dir" "$cache_dir"

    if check_remaining_installations; then
        rm -rf "$HOME/.superzent_server" "$HOME/.superzent_wsl_server"
    fi

    prompt_remove_preferences "$config_dir"
}

macos() {
    name="$(app_dir_name)"
    bundle_id="$(mac_bundle_id)"
    app_bundle="$(mac_app_bundle)"
    config_dir="$HOME/.config/$name"

    if [ -d "/Applications/$app_bundle" ]; then
        rm -rf "/Applications/$app_bundle"
    fi

    rm -f "$HOME/.local/bin/superzent"
    rm -rf "$HOME/Library/Application Support/$name"
    rm -rf "$HOME/.local/state/$name"
    rm -rf "$HOME/Library/Logs/$name"
    rm -rf "$HOME/Library/Caches/$name"
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$bundle_id.sfl"*
    rm -rf "$HOME/Library/Caches/$bundle_id"
    rm -rf "$HOME/Library/HTTPStorages/$bundle_id"
    rm -rf "$HOME/Library/Preferences/$bundle_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$bundle_id.savedState"

    if check_remaining_installations; then
        rm -rf "$HOME/.superzent_server" "$HOME/.superzent_wsl_server"
    fi

    prompt_remove_preferences "$config_dir"
}

main "$@"
