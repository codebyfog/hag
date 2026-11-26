#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
install_dir="$data_home/hag"
desktop_dir="$data_home/applications"
bin_dir="$HOME/.local/bin"

cargo build --release --locked --target-dir target
install -d "$install_dir" "$desktop_dir" "$bin_dir"
install -m 0755 target/release/hag "$bin_dir/hag"
cp -R icons "$install_dir/"
app_icons="$data_home/icons/hicolor/scalable/apps"
install -Dm644 icons/hicolor/scalable/apps/HAG.svg "$app_icons/io.github.codebyfog.HAG.svg"
{
    while IFS= read -r line; do
        if [[ "$line" == Exec=* ]]; then
            printf 'Exec="%s"\n' "$bin_dir/hag"
        else
            printf '%s\n' "$line"
        fi
    done < io.github.codebyfog.HAG.desktop
} > "$desktop_dir/io.github.codebyfog.HAG.desktop"

# Retire the previous launcher only when it points to our old local install.
legacy_launcher="$desktop_dir/com.github.helix-ui.desktop"
if [[ -f "$legacy_launcher" ]] && grep -Fqx "Exec=\"$bin_dir/helix-ui\"" "$legacy_launcher"; then
    rm -- "$legacy_launcher"
fi

# Retire the launcher from the previous application ID.
previous_launcher="$desktop_dir/dev.victorsoares.HAG.desktop"
if [[ -f "$previous_launcher" ]] && grep -Fqx "Exec=\"$bin_dir/hag\"" "$previous_launcher"; then
    rm -- "$previous_launcher"
    rm -f -- "$app_icons/dev.victorsoares.HAG.svg"
fi

if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$desktop_dir"
fi
printf 'Installed HAG at %s\n' "$bin_dir/hag"
