#!/usr/bin/env bash
set -euo pipefail

# Ensure we're in the repository root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_DIR"

OUTPUT="preview.png"
WIDTH=680
HEIGHT=440
SEED=true
ACTIONS=()
CUSTOM_DISPLAY=""

print_usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Run copypest inside a headless X virtual framebuffer (Xvfb), optionally seed
clipboard history, send keyboard/mouse inputs, and capture a screenshot.

Options:
  -o, --output <FILE>     Output path for the screenshot PNG (default: preview.png)
  -w, --width <INT>       Window / framebuffer width (default: 680)
  -h, --height <INT>      Window / framebuffer height (default: 440)
  --no-seed               Do not populate initial clipboard test items
  -k, --key <KEY>         Send key via xdotool (e.g. Down, Up, Return, Delete, Escape)
  -t, --type <TEXT>       Type text string into focused input (search bar)
  -s, --sleep <SEC>       Sleep for given duration in seconds (e.g. 0.5)
  --click <X> <Y>         Click mouse at coordinates X Y
  --script <BASH>         Run arbitrary bash commands before screenshot
  -d, --display <NUM>     X display number (auto-selects free display >= 99 if omitted)
  --help                  Show this help message

Examples:
  # Basic screenshot with seeded items
  ./scripts/preview.sh -o /tmp/main.png

  # Navigate down and capture
  ./scripts/preview.sh -k Down -k Down -o /tmp/selected.png

  # Test search filtering
  ./scripts/preview.sh -t "cargo" -o /tmp/search.png

  # Test confirmation badge on Return
  ./scripts/preview.sh -k Return -o /tmp/confirm.png
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -o|--output)
            OUTPUT="$2"
            shift 2
            ;;
        -w|--width)
            WIDTH="$2"
            shift 2
            ;;
        -h|--height)
            HEIGHT="$2"
            shift 2
            ;;
        --no-seed)
            SEED=false
            shift
            ;;
        -k|--key)
            ACTIONS+=("key:$2")
            shift 2
            ;;
        -t|--type)
            ACTIONS+=("type:$2")
            shift 2
            ;;
        -s|--sleep)
            ACTIONS+=("sleep:$2")
            shift 2
            ;;
        --click)
            ACTIONS+=("click:$2:$3")
            shift 3
            ;;
        --script)
            ACTIONS+=("script:$2")
            shift 2
            ;;
        -d|--display)
            CUSTOM_DISPLAY="$2"
            shift 2
            ;;
        --help)
            print_usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            print_usage >&2
            exit 1
            ;;
    esac
done

# Find a free display if not specified
if [[ -n "$CUSTOM_DISPLAY" ]]; then
    DISPLAY_NUM="$CUSTOM_DISPLAY"
else
    DISPLAY_NUM=99
    while [[ -e "/tmp/.X11-unix/X${DISPLAY_NUM}" ]]; do
        DISPLAY_NUM=$((DISPLAY_NUM + 1))
    done
fi

export DISPLAY=":${DISPLAY_NUM}"
export XDG_RUNTIME_DIR="$(mktemp -d "/tmp/copypest-test-XXXXXX")"

# Ensure binary is built
cargo build --bin copypest --quiet
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
BINARY="${TARGET_DIR}/debug/copypest"

if [[ ! -x "$BINARY" ]]; then
    echo "Error: binary not found at $BINARY" >&2
    exit 1
fi

XVFB_PID=""
APP_PID=""

cleanup() {
    if [[ -n "${UI_PID:-}" ]]; then
        kill "$UI_PID" 2>/dev/null || true
    fi
    if [[ -n "$APP_PID" ]]; then
        kill "$APP_PID" 2>/dev/null || true
    fi
    if [[ -n "$XVFB_PID" ]]; then
        kill "$XVFB_PID" 2>/dev/null || true
    fi
    if [[ -d "$XDG_RUNTIME_DIR" ]]; then
        rm -rf "$XDG_RUNTIME_DIR"
    fi
}
trap cleanup EXIT

# 1. Start Xvfb framebuffer matching the window inner size
Xvfb "$DISPLAY" -screen 0 "${WIDTH}x${HEIGHT}x24" -ac +extension GLX +render -noreset 2>/dev/null &
XVFB_PID=$!

# Wait for X server to be ready
for _ in {1..50}; do
    if xdotool getdisplaygeometry >/dev/null 2>&1; then
        break
    fi
    sleep 0.05
done

# 2. Start daemon in background to capture clipboard events
"$BINARY" daemon &
APP_PID=$!
sleep 0.5

# 3. Seed sample clipboard data if requested
if [[ "$SEED" == "true" ]]; then
    # Text item
    echo "Antigravity Clipboard Manager" | xclip -selection clipboard
    sleep 0.1

    # Files URI list
    printf "file://%s/Cargo.toml\nfile://%s/flake.nix\n" "$REPO_DIR" "$REPO_DIR" | xclip -selection clipboard -t text/uri-list
    sleep 0.1

    # Image item (generate a clean sample image)
    SAMPLE_IMG="$(mktemp "/tmp/copypest-sample-XXXXXX.png")"
    magick -size 120x80 gradient:#6366f1-#ec4899 -font DejaVu-Sans-Bold -pointsize 18 -fill white -gravity center -annotate +0+0 "PREVIEW" "$SAMPLE_IMG" 2>/dev/null || \
    magick -size 120x80 xc:#6366f1 "$SAMPLE_IMG"
    xclip -selection clipboard -t image/png "$SAMPLE_IMG"
    rm -f "$SAMPLE_IMG"
    sleep 0.1

    # Code / URL snippet
    echo "https://github.com/rust-lang/rust" | xclip -selection clipboard
    sleep 0.1
fi

# 4. Show the UI window
"$BINARY" &
UI_PID=$!
sleep 0.5

# Find window and focus it
WIN=""
for _ in {1..30}; do
    WIN=$(xdotool search --name copypest 2>/dev/null | tail -1 || true)
    if [[ -n "$WIN" ]]; then
        xdotool windowfocus --sync "$WIN" 2>/dev/null || true
        xdotool windowactivate --sync "$WIN" 2>/dev/null || true
        break
    fi
    sleep 0.05
done

# 5. Move mouse cursor off-screen so it is not visible in the screenshot
xdotool mousemove 9999 9999 2>/dev/null || true
sleep 0.2

# 6. Execute requested actions
for action in "${ACTIONS[@]}"; do
    case "$action" in
        key:*)
            k="${action#key:}"
            xdotool key "$k"
            sleep 0.2
            ;;
        type:*)
            t="${action#type:}"
            xdotool type --delay 20 "$t"
            sleep 0.2
            ;;
        sleep:*)
            s="${action#sleep:}"
            sleep "$s"
            ;;
        click:*)
            coords="${action#click:}"
            cx="${coords%%:*}"
            cy="${coords#*:}"
            xdotool mousemove "$cx" "$cy" click 1
            sleep 0.2
            ;;
        script:*)
            sc="${action#script:}"
            eval "$sc"
            sleep 0.2
            ;;
    esac
done

# Small settle delay before capture
sleep 0.3

# 7. Capture screenshot
mkdir -p "$(dirname "$OUTPUT")"
maim -u "$OUTPUT"

echo "Preview screenshot saved to: $OUTPUT"
