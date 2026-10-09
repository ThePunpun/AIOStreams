@echo off
setlocal
cd /d "%~dp0"
set "AIOSTREAMS_PREVIEW_TEST_VARIANT="
set "AIOSTREAMS_PREVIEW_USENET_BACKGROUND=off"
set "AIOSTREAMS_PREVIEW_COST_LEARNING="
start "" "%~dp0AIOStreams-Custom.exe"
