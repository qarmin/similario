#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$DIR/tests/videos"
mkdir -p "$OUT"

URLS=(
    "https://youtu.be/JU0B662T-vY"
    "https://youtu.be/MGdn0qtrsrE"
    "https://youtu.be/8zNLCVroj2A"
    "https://youtu.be/TaUlBYqGuiE"
)

# --- Hardware acceleration setup (VAAPI for AMD GPUs) ---
VAAPI_DEV="/dev/dri/renderD128"
HW_ENCODE_ARGS=()
HW_SCALE_PREFIX=""

if [[ -c "$VAAPI_DEV" ]] && vainfo --display drm --device "$VAAPI_DEV" &>/dev/null; then
    echo "=== Using VAAPI hardware encoding ($(vainfo --display drm --device "$VAAPI_DEV" 2>&1 | grep 'Driver version' | sed 's/.*: //')) ==="
    HW_INIT=(-vaapi_device "$VAAPI_DEV")
    HW_UPLOAD=",format=nv12,hwupload"
    HW_ENCODE=(-c:v h264_vaapi -qp 24)
    HW_AVAIL=1
else
    echo "=== No VAAPI device found, using software encoding ==="
    HW_INIT=()
    HW_UPLOAD=""
    HW_ENCODE=(-c:v libx264 -preset fast)
    HW_AVAIL=0
fi

# Helper: encode with VAAPI or software fallback.
# Usage: hw_encode INPUT OUTPUT [extra_vf_before_upload] [extra_ffmpeg_args...]
#   extra_vf_before_upload: filter chain to apply BEFORE hwupload (e.g. "scale=-2:480")
#   If empty, no scaling/filter is applied.
hw_encode() {
    local input="$1" output="$2" vf_pre="${3:-}" ; shift 3

    if (( HW_AVAIL )); then
        local vf=""
        if [[ -n "$vf_pre" ]]; then
            vf="${vf_pre}${HW_UPLOAD}"
        else
            vf="format=nv12,hwupload"
        fi
        ffmpeg -y "${HW_INIT[@]}" -i "$input" -vf "$vf" "${HW_ENCODE[@]}" "$@" "$output" 2>/dev/null
    else
        local vf="$vf_pre"
        if [[ -n "$vf" ]]; then
            ffmpeg -y -i "$input" -vf "$vf" "${HW_ENCODE[@]}" "$@" "$output" 2>/dev/null
        else
            ffmpeg -y -i "$input" "${HW_ENCODE[@]}" "$@" "$output" 2>/dev/null
        fi
    fi
}

echo "=== Downloading original videos ==="
for url in "${URLS[@]}"; do
    yt-dlp \
        --no-playlist \
        --format "bestvideo[height<=720][ext=mp4]+bestaudio[ext=m4a]/best[height<=720][ext=mp4]/best" \
        --merge-output-format mp4 \
        --output "$OUT/%(id)s.%(ext)s" \
        --no-overwrites \
        "$url"
done

echo ""
echo "=== Generating variants ==="

for orig in "$OUT"/*.mp4; do
    base="$(basename "$orig" .mp4)"

    # Skip files that are already variants
    [[ "$base" == *_* ]] && continue

    echo "--- Variants for $base ---"

    # 1. Re-encoded (same content, different codec params) → should be SameContent/Identical
    if [[ ! -f "$OUT/${base}_reencode.mp4" ]]; then
        echo "  re-encode"
        hw_encode "$orig" "$OUT/${base}_reencode.mp4" "" -c:a aac -b:a 96k
    fi

    # 2. Lower resolution (480p) → should be SameContent/Similar
    if [[ ! -f "$OUT/${base}_480p.mp4" ]]; then
        echo "  480p"
        hw_encode "$orig" "$OUT/${base}_480p.mp4" "scale=-2:480" -c:a copy
    fi

    # 3. Lower resolution (240p) → should be Similar
    if [[ ! -f "$OUT/${base}_240p.mp4" ]]; then
        echo "  240p"
        hw_encode "$orig" "$OUT/${base}_240p.mp4" "scale=-2:240" -c:a copy
    fi

    # 4. Trimmed first half → should be SubClip (stream copy, instant)
    duration=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$orig" | head -1)
    half=$(echo "$duration / 2" | bc -l | xargs printf "%.2f")
    if [[ ! -f "$OUT/${base}_trimmed.mp4" ]]; then
        echo "  trimmed (first ${half}s)"
        ffmpeg -y -i "$orig" -t "$half" -c copy \
            "$OUT/${base}_trimmed.mp4" 2>/dev/null
    fi

    # 5. With letterbox bars (pillarbox) → should be SameContent/Similar
    if [[ ! -f "$OUT/${base}_bars.mp4" ]]; then
        echo "  letterbox bars"
        hw_encode "$orig" "$OUT/${base}_bars.mp4" \
            "scale=iw*0.75:-2,pad=iw/0.75:ih:(ow-iw)/2:(oh-ih)/2:black" -c:a copy
    fi

    # 6. Heavy compression (low quality) → should be Similar
    if [[ ! -f "$OUT/${base}_lowq.mp4" ]]; then
        echo "  low quality"
        if (( HW_AVAIL )); then
            ffmpeg -y "${HW_INIT[@]}" -i "$orig" \
                -vf "format=nv12,hwupload" -c:v h264_vaapi -qp 45 \
                -c:a aac -b:a 48k \
                "$OUT/${base}_lowq.mp4" 2>/dev/null
        else
            ffmpeg -y -i "$orig" -c:v libx264 -crf 40 -preset ultrafast \
                -c:a aac -b:a 48k \
                "$OUT/${base}_lowq.mp4" 2>/dev/null
        fi
    fi

    # 7. Cropped center (80%) → should be Similar
    if [[ ! -f "$OUT/${base}_cropped.mp4" ]]; then
        echo "  cropped center"
        hw_encode "$orig" "$OUT/${base}_cropped.mp4" "crop=iw*0.8:ih*0.8" -c:a copy
    fi

    # 8. Different container (mkv, no re-encode) → should be Identical/SameContent (instant)
    if [[ ! -f "$OUT/${base}_remux.mkv" ]]; then
        echo "  remux to mkv"
        ffmpeg -y -i "$orig" -c copy \
            "$OUT/${base}_remux.mkv" 2>/dev/null
    fi
done

echo ""
echo "=== Summary ==="
echo "Videos in $OUT:"
ls -1 "$OUT" | head -60
echo "Total: $(ls -1 "$OUT" | wc -l) files"
