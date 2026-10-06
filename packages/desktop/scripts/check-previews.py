"""Exercise the real thumbnail decoder against authenticated HTTP Range streams."""
import http.server
import json
import os
import pathlib
import re
import subprocess
import tempfile
import threading


def check_fixture(name, extension, encoder, gamma=None):
    with tempfile.TemporaryDirectory(prefix="aio-preview-test-") as temporary:
        directory = pathlib.Path(temporary)
        media = directory / f"fixture.{extension}"
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", "testsrc2=size=640x360:rate=24", "-t", "12", *encoder,
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
                    self.wfile.write(data[start:end + 1])
                    counts["bytes"] += end - start + 1
                except (BrokenPipeError, ConnectionResetError):
                    pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        environment = os.environ.copy()
        environment["PREVIEW_TEST_URL"] = f"http://127.0.0.1:{server.server_port}/fixture.{extension}?fixture-secret=redacted"
        environment["PREVIEW_TEST_OUTPUT"] = temporary
        environment.pop("PREVIEW_TEST_EXPECT_GAMMA", None)
        if gamma:
            environment["PREVIEW_TEST_EXPECT_GAMMA"] = gamma
        try:
            subprocess.run(["cargo", "test", "-p", "aiostreams-desktop-core", "remote_decoder_and_session_cleanup", "--", "--ignored", "--nocapture"], env=environment, check=True)
            for image_name in ("first.jpg", "later.jpg"):
                result = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", str(directory / image_name)], capture_output=True, text=True, check=True)
                assert result.stdout.strip() == "320,180", result.stdout
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
