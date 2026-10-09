@echo off
setlocal
cd /d "%~dp0"
set "AIOSTREAMS_PREVIEW_TEST_VARIANT=fast-seek"
start "" "%~dp0AIOStreams-Custom.exe"
