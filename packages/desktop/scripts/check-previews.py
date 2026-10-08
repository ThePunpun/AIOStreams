"""Exercise the real thumbnail decoder against authenticated HTTP Range streams."""
import http.server
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import threading
import time


def check_fixture(name, extension, encoder, gamma=None, size="640x360", rate=24, landing=16, delay=0, duration=36):
    if selected := os.environ.get("PREVIEW_TEST_FILTER"):
        if selected.lower() not in name.lower():
            return
    with tempfile.TemporaryDirectory(prefix="aio-preview-test-") as temporary:
        directory = pathlib.Path(temporary)
        media = directory / f"fixture.{extension}"
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", f"testsrc2=size={size}:rate={rate}", "-t", str(duration), *encoder,
            str(media),
        ], check=True)
        probe = subprocess.run([
            "ffprobe", "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=pix_fmt,color_transfer,color_primaries,color_space",
            "-of", "json", str(media),
        ], capture_output=True, text=True, check=True)
        properties = json.loads(probe.stdout)["streams"][0]
        print(f"Encoded fixture ({name}): {properties}", flush=True)
        if gamma == "pq":
            assert properties.get("color_transfer") == "smpte2084", properties
            assert properties.get("pix_fmt") == "yuv420p10le", properties
        data = media.read_bytes()
        counts = {"ranges": 0, "requests": 0, "bytes": 0, "rejected": 0}

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.headers.get("Authorization") != "Bearer preview-fixture":
                    counts["rejected"] += 1
                    self.send_error(401)
                    return
                counts["requests"] += 1
                start, end = 0, len(data) - 1
                range_header = self.headers.get("Range")
                if range_header:
                    match = re.fullmatch(r"bytes=(\d+)-(\d*)", range_header)
                    if not match:
                        self.send_error(416)
                        return
                    start = int(match[1])
                    if match[2]:
                        end = min(end, int(match[2]))
                    if start > end:
                        self.send_error(416)
                        return
                    counts["ranges"] += 1
                self.send_response(206 if range_header else 200)
                self.send_header("Content-Type", "video/mp4" if extension == "mp4" else "video/x-matroska")
                self.send_header("Accept-Ranges", "bytes")
                self.send_header("Content-Length", str(end - start + 1))
                if range_header:
                    self.send_header("Content-Range", f"bytes {start}-{end}/{len(data)}")
                self.end_headers()
                try:
                    if delay:
                        time.sleep(delay)
                    self.wfile.write(data[start:end + 1])
                    counts["bytes"] += end - start + 1
                except (BrokenPipeError, ConnectionResetError):
                    pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        environment = os.environ.copy()
        environment["PREVIEW_TEST_URL"] = f"http://127.0.0.1:{server.server_port}/fixture.{extension}?fixture-secret=redacted"
        environment["PREVIEW_TEST_OUTPUT"] = temporary
        environment["PREVIEW_TEST_EXPECT_LANDING"] = str(landing)
        environment["PREVIEW_TEST_STEP"] = "8" if duration > 3600 else "5"
        environment["PREVIEW_TEST_REQUIRE_PREEMPT"] = "yes" if "slow Range 4K" in name else "no"
        environment["PREVIEW_TEST_AUTO_OPEN"] = "yes" if name == "H.264 MP4" else "no"
        environment.pop("PREVIEW_TEST_EXPECT_GAMMA", None)
        if gamma:
            environment["PREVIEW_TEST_EXPECT_GAMMA"] = gamma
        try:
            subprocess.run(["cargo", "test", "-p", "aiostreams-desktop-core", "remote_decoder_and_session_cleanup", "--", "--ignored", "--nocapture"], env=environment, check=True, cwd=pathlib.Path(__file__).resolve().parents[1])
            for image_name in ("first.jpg", "later.jpg"):
                result = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", str(directory / image_name)], capture_output=True, text=True, check=True)
                source_width, source_height = map(int, size.split("x"))
                expected_height = round(320 * source_height / source_width / 2) * 2
                image_width, image_height = map(int, result.stdout.strip().split(","))
                # mpv screenshots apply pixel-aspect correction after even-height scaling.
                assert image_height == expected_height and 318 <= image_width <= 324, result.stdout
                assert abs(image_width / image_height - source_width / source_height) <= 1 / image_height, result.stdout
            if gamma == "pq":
                def rgb(path):
                    return subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], capture_output=True, check=True).stdout
                old = rgb(directory / "baseline.jpg")
                new = rgb(directory / "first.jpg")
                assert len(old) == len(new) == 320 * 180 * 3
                error = sum(abs(a-b) for a,b in zip(old,new)) / len(old)
                assert error < 15, f"HDR known-good reference mismatch: mean RGB error {error:.2f}/255"
                print(f"HDR known-good reference comparison: mean RGB error {error:.2f}/255", flush=True)
                if archive := os.environ.get("PREVIEW_TEST_ARCHIVE"):
                    destination = pathlib.Path(archive)
                    destination.mkdir(parents=True, exist_ok=True)
                    for image in ("first.jpg", "baseline.jpg"):
                        shutil.copyfile(directory / image, destination / f"{size}-{image}")
            assert counts["ranges"] > 0, "remote seeking did not use Range requests"
            assert counts["rejected"] == 0, "thumbnail worker lost the playback headers"
            print(f"Authenticated Range fixture passed ({name}): {counts}")
        finally:
            server.shutdown()
            server.server_close()


check_fixture("H.264 MP4", "mp4", [
    "-c:v", "libx264", "-g", "48", "-pix_fmt", "yuv420p",
    "-movflags", "+faststart",
])
hevc = [
    "-c:v", "libx265", "-preset", "ultrafast", "-x265-params",
    "pools=1:frame-threads=1:log-level=error", "-g", "48",
]
check_fixture("HEVC MKV", "mkv", [*hevc, "-pix_fmt", "yuv420p"])
check_fixture("10-bit HEVC PQ MKV", "mkv", [
    *hevc, "-pix_fmt", "yuv420p10le", "-color_trc", "smpte2084",
    "-color_primaries", "bt2020", "-colorspace", "bt2020nc",
    "-vf", "setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
], gamma="pq")

check_fixture("4K 10-bit HEVC PQ MKV", "mkv", [
    "-c:v", "libx265", "-preset", "ultrafast", "-x265-params",
    "pools=1:frame-threads=1:log-level=error", "-g", "4",
    "-pix_fmt", "yuv420p10le", "-color_trc", "smpte2084",
    "-color_primaries", "bt2020", "-colorspace", "bt2020nc",
    "-vf", "setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
], gamma="pq", size="3840x2160", rate=2)

check_fixture("slow Range H.264, 10-second GOP", "mp4", [
    "-c:v", "libx264", "-g", "240", "-keyint_min", "240",
    "-sc_threshold", "0", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
], landing=10, delay=0.35)

check_fixture("slow Range 4K PQ, 10-second GOP", "mkv", [
    "-c:v", "libx265", "-preset", "ultrafast", "-x265-params",
    "pools=1:frame-threads=1:log-level=error:keyint=20:min-keyint=20:scenecut=0",
    "-g", "20", "-pix_fmt", "yuv420p10le", "-color_trc", "smpte2084",
    "-color_primaries", "bt2020", "-colorspace", "bt2020nc",
    "-vf", "setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
], gamma="pq", size="3840x2160", rate=2, landing=10, delay=0.35)

check_fixture("H.264 4:3 MP4", "mp4", [
    "-c:v", "libx264", "-g", "48", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
], size="640x480")
check_fixture("H.264 21:9 MP4", "mp4", [
    "-c:v", "libx264", "-g", "48", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
], size="840x360")
check_fixture("10-bit HEVC HLG MKV", "mkv", [
    *hevc, "-pix_fmt", "yuv420p10le", "-color_trc", "arib-std-b67",
    "-color_primaries", "bt2020", "-colorspace", "bt2020nc",
    "-vf", "setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc",
], gamma="hlg")

# Real long-duration metadata exercises the production 8-second scheduler and UI protocol.
check_fixture("long H.264 MP4, 8-second grid", "mp4", [
    "-c:v", "libx264", "-preset", "ultrafast", "-g", "2", "-sc_threshold", "0",
    "-pix_fmt", "yuv420p", "-movflags", "+faststart",
], rate=1, duration=3608, landing=12)
