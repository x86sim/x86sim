#!/bin/bash
# webcov: the suite's media and font fixtures, generated with ffmpeg (not committed): 2 s clips with a known picture
# (solid red top half, blue bottom half: the checks read the decoded pixels back) and a 440 Hz tone.
# Usage: gen-media.sh OUTDIR   (skips the work when OUTDIR/.done is newer than this script)
set -eu
D=${1:?usage: gen-media.sh OUTDIR}; mkdir -p "$D/hls"
[ "$D/.done" -nt "$0" ] && exit 0
# every ffmpeg run is bounded: 3 GB of address space and 120 s (an unbounded lavfi source into a filter that waits for
# EOF -- palettegen -- once buffered frames until the host ran out of memory). Every source below has a duration.
TO=; command -v timeout >/dev/null && TO="timeout 120"
ff() { ( ulimit -v 3000000 2>/dev/null || true; exec $TO nice ffmpeg -hide_banner -loglevel error -nostdin -threads 2 -y "$@" ); }
# red over blue, 320x240, 30 fps
V='color=c=red:s=320x120:r=30:d=2[a];color=c=blue:s=320x120:r=30:d=2[b];[a][b]vstack'
A='sine=frequency=440:duration=2:sample_rate=48000'
ff -filter_complex "$V" -f lavfi -i "$A" -c:v libx264 -profile:v main -pix_fmt yuv420p -c:a aac -b:a 96k -movflags +faststart "$D/h264.mp4"
ff -filter_complex "$V" -f lavfi -i "$A" -c:v libx265 -tag:v hvc1 -pix_fmt yuv420p -x265-params log-level=error -c:a aac -movflags +faststart "$D/hevc.mp4"
ff -filter_complex "$V" -c:v libvpx-vp9 -b:v 300k -an "$D/vp9.webm"
ff -f lavfi -i "$A" -c:a aac -b:a 96k "$D/aac.m4a"
ff -f lavfi -i "$A" -c:a libmp3lame -b:a 96k "$D/mp3.mp3"
ff -f lavfi -i "$A" -c:a libopus -b:a 64k "$D/opus.webm"
ff -f lavfi -i "$A" -c:a libopus -b:a 64k "$D/opus.mp4"
ff -f lavfi -i "$A" -c:a flac "$D/flac.flac"
ff -f lavfi -i "$A" -c:a pcm_s16le "$D/pcm.wav"
# MSE / ManagedMediaSource: fragmented MP4, video only and audio only (one SourceBuffer each)
ff -filter_complex "$V" -c:v libx264 -profile:v main -pix_fmt yuv420p -g 30 -an -movflags frag_keyframe+empty_moov+default_base_moof "$D/frag-v.mp4"
ff -f lavfi -i "$A" -c:a aac -b:a 96k -movflags frag_keyframe+empty_moov+default_base_moof "$D/frag-a.m4a"
# HLS: H.264 + AAC in TS segments of 1 s
ff -filter_complex "$V" -f lavfi -i "$A" -c:v libx264 -profile:v main -pix_fmt yuv420p -g 30 -c:a aac -b:a 96k \
	-f hls -hls_time 1 -hls_playlist_type vod -hls_segment_filename "$D/hls/seg%d.ts" "$D/hls/index.m3u8"
# still images: the same red-over-blue picture in each format (decode checks read pixels back)
I='color=c=red:s=64x32:r=1:d=1[a];color=c=blue:s=64x32:r=1:d=1[b];[a][b]vstack'
ff -filter_complex "$I" -frames:v 1 "$D/img.png"
ff -filter_complex "$I" -frames:v 1 -q:v 2 "$D/img.jpg"
ff -filter_complex "$I" -frames:v 1 "$D/img.webp"
ff -filter_complex "$I,split[x][y];[x]palettegen[p];[y][p]paletteuse" -frames:v 1 "$D/img.gif"
ff -filter_complex "$I" -frames:v 1 -c:v libaom-av1 -still-picture 1 -pix_fmt yuv420p "$D/img.avif" 2>/dev/null || echo "gen-media: no AVIF encoder" >&2
command -v heif-enc >/dev/null && heif-enc -q 90 -o "$D/img.heic" "$D/img.png" >/dev/null 2>&1
[ -f "$D/img.heic" ] || echo "gen-media: no HEIC (heif-enc)" >&2
# a web font (FontFace / @font-face checks): a free font of the host, by family
for f in /usr/share/fonts/TTF/DejaVuSansMono.ttf /usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf \
	$(fc-match -f '%{file}' 'DejaVu Sans Mono' 2>/dev/null); do [ -f "$f" ] && { cp "$f" "$D/webfont.ttf"; break; }; done
[ -f "$D/webfont.ttf" ] || echo "gen-media: no DejaVu Sans Mono for the web font check" >&2
touch "$D/.done"
