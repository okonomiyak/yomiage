@echo off
setlocal enabledelayedexpansion
REM ニコニコの Cookie (NICO_COOKIES) を差し替えて疎通確認する。
REM
REM   scripts\update-nico-cookies.bat <エクスポートした cookies.txt>
REM
REM ブラウザ拡張機能（Get cookies.txt 等）で nicovideo.jp にログイン済みの状態から
REM エクスポートした Netscape 形式の Cookie ファイルを渡すこと。
REM 環境変数で上書きできる: PVE_HOST / CTID / DEST
REM
REM 再起動は不要。yt-dlp は呼び出しのたびにファイルを読むだけなので、
REM 差し替えた次の再生から反映される。
REM
REM Windows 10/11 標準の OpenSSH クライアント（ssh.exe / scp.exe）が要る。

if "%~1"=="" (
    echo 使い方: scripts\update-nico-cookies.bat ^<cookies.txt へのパス^>
    exit /b 1
)
set "COOKIES_FILE=%~1"
if not exist "%COOKIES_FILE%" (
    echo ファイルが見つからない: %COOKIES_FILE%
    exit /b 1
)

if exist .env (
    for /f "usebackq tokens=1,* delims==" %%A in (`findstr /b "PVE_HOST=" .env`) do if not defined PVE_HOST set "PVE_HOST=%%B"
    for /f "usebackq tokens=1,* delims==" %%A in (`findstr /b "CTID=" .env`) do if not defined CTID set "CTID=%%B"
    for /f "usebackq tokens=1,* delims==" %%A in (`findstr /b "DEST=" .env`) do if not defined DEST set "DEST=%%B"
)
if not defined PVE_HOST (
    echo PVE_HOST を .env に設定してください（例: root@10.0.0.1^）
    exit /b 1
)
if not defined CTID (
    echo CTID を .env に設定してください（例: 110^）
    exit /b 1
)
if not defined DEST set "DEST=/opt/yomiage"

set "REMOTE_TMP=/tmp/nico_cookies_%RANDOM%.txt"

echo == 転送 %COOKIES_FILE% -^> %PVE_HOST% CT%CTID%:%DEST%/data/nico_cookies.txt
scp "%COOKIES_FILE%" "%PVE_HOST%:%REMOTE_TMP%"
if errorlevel 1 exit /b 1

REM 差し替える前に今のファイルを 1 世代だけ退避する。差し替えた内容が壊れていた場合に
REM 手で戻せるようにするため（新しいものを試すたびに前のものが消えると戻せない）。
ssh "%PVE_HOST%" "pct exec %CTID% -- sh -c 'test -f %DEST%/data/nico_cookies.txt && cp %DEST%/data/nico_cookies.txt %DEST%/data/nico_cookies.txt.bak || true'"

ssh "%PVE_HOST%" "pct push %CTID% %REMOTE_TMP% %DEST%/data/nico_cookies.txt --perms 600 && rm -f %REMOTE_TMP%"
if errorlevel 1 exit /b 1

echo == 疎通確認（コピーで確認するので置いた原本には触れない）
echo    動画のメタデータ (JSON) が出れば成功。
echo    'ERROR: ... Invalid session' が出たら、ログインし直してエクスポートし直すこと。
ssh "%PVE_HOST%" "pct exec %CTID% -- sh -c 'cp %DEST%/data/nico_cookies.txt /tmp/nico_verify.txt && docker cp /tmp/nico_verify.txt yomiage-bot:/tmp/nico_verify.txt && docker exec yomiage-bot yt-dlp --no-playlist --no-warnings -j --cookies /tmp/nico_verify.txt https://www.nicovideo.jp/watch/sm9; docker exec yomiage-bot rm -f /tmp/nico_verify.txt; rm -f /tmp/nico_verify.txt'"

endlocal
