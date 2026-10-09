@echo off
setlocal
cd /d "%~dp0"
set "AIOSTREAMS_PREVIEW_TEST_VARIANT="
set "AIOSTREAMS_PREVIEW_USENET_BACKGROUND="
set "AIOSTREAMS_PREVIEW_COST_LEARNING=off"
start "" "%~dp0AIOStreams-Custom.exe"
