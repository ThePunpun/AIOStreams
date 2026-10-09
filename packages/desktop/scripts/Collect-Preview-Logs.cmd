@echo off
setlocal
cd /d "%~dp0"
echo Close AIOStreams Custom before collecting its logs.
echo Only dedicated seek-preview logs are included.
powershell.exe -NoProfile -Command "$ErrorActionPreference = 'Stop'; try { $logs = @(Get-ChildItem -LiteralPath '.\data\logs' -Filter 'seek-previews-*.log' -File); if ($logs.Count -eq 0) { throw 'No preview logs found. Run a preview test first.' }; Compress-Archive -LiteralPath $logs.FullName -DestinationPath '.\Preview-Logs.zip' -Force; Write-Host ('Created Preview-Logs.zip with ' + $logs.Count + ' logs. Attach this ZIP to your message.') } catch { Write-Host $_.Exception.Message; exit 1 }"
pause
