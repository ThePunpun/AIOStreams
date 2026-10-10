"""Verify the experimental filter with a real P5 stream; unavailable is explicit."""
import json
import os
import pathlib
import subprocess
import tempfile
import urllib.request

SAMPLE = "https://media.developer.dolby.com/DolbyVision_Atmos/mp4/iOS_P5_GlassBlowing2_3840x2160%4059.94fps_15200kbps.mp4"
with tempfile.TemporaryDirectory(prefix="aio-real-p5-") as temporary:
    directory = pathlib.Path(temporary)
    source = os.environ.get("PREVIEW_TEST_P5_FILE")
    if not source:
        source = str(directory / "source.mp4")
        with urllib.request.urlopen(SAMPLE, timeout=60) as response, open(source, "wb") as output:
            while block := response.read(1024 * 1024):
                output.write(block)
    fixture = pathlib.Path(source)
    probe = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_streams", "-of", "json", str(fixture)], capture_output=True, text=True, check=True)
    stream = json.loads(probe.stdout)["streams"][0]
    assert any(record.get("dv_profile") == 5 and record.get("rpu_present_flag") == 1 for record in stream.get("side_data_list", [])), "fixture lost real P5 metadata"
    environment = os.environ.copy()
    environment["PREVIEW_TEST_P5_FILE"] = str(fixture)
    subprocess.run(["cargo", "test", "-p", "aiostreams-desktop-core", "dv5_conversion_or_bounded_unavailable", "--", "--ignored", "--nocapture"], env=environment, check=True, cwd=pathlib.Path(__file__).resolve().parents[1])
