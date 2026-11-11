@echo off
chcp 65001 >nul
setlocal EnableExtensions

rem コミット済みのツリーを LXC へ送ってビルド・再起動する。deploy.sh の Windows 版。
rem
rem   scripts\deploy.bat
rem
rem 環境変数で上書きできる: PVE_HOST / CTID / DEST / COMPOSE_FILE
rem 事前に LXC の DEST にある .env に TTS_DISCORD_TOKEN と MUSIC_DISCORD_TOKEN を
rem 置いておくこと。このスクリプトは触らない。
rem
rem COMPOSE_FILE は既定で compose.yaml、本番の GPU 版。CPU 専用機へ配るときは
rem   set COMPOSE_FILE=compose.cpu.yaml
rem   scripts\deploy.bat
rem のように指定する。
rem
rem 前提: Git for Windows と OpenSSH クライアントが PATH にあること。

rem リポジトリのルートへ移動する。どこから実行しても動くようにするため。
pushd "%~dp0.."
if errorlevel 1 (
    echo エラー: リポジトリのルートへ移動できません。 1>&2
    exit /b 1
)

rem 接続先は .env（git 管理外）から読む。公開リポジトリに自宅サーバーのアドレスを
rem 置かないため。環境変数で直接渡してもよい。
if exist .env (
    for /f "usebackq tokens=1,* delims==" %%A in (".env") do (
        if "%%A"=="PVE_HOST" if not defined PVE_HOST set "PVE_HOST=%%B"
        if "%%A"=="CTID" if not defined CTID set "CTID=%%B"
        if "%%A"=="DEST" if not defined DEST set "DEST=%%B"
    )
)

if not defined PVE_HOST (
    echo エラー: PVE_HOST を .env に設定してください。例: root@10.0.0.1 1>&2
    goto :fail
)
if not defined CTID (
    echo エラー: CTID を .env に設定してください。例: 110 1>&2
    goto :fail
)
if not defined DEST set "DEST=/opt/yomiage"
if not defined COMPOSE_FILE set "COMPOSE_FILE=compose.yaml"

git diff --quiet HEAD >nul 2>&1
if errorlevel 1 echo 警告: コミットしていない変更がある。送られるのは HEAD の内容だけ。 1>&2

set "REV="
for /f %%i in ('git rev-parse --short HEAD 2^>nul') do set "REV=%%i"
if not defined REV (
    echo エラー: git リポジトリではないか、コミットがありません。 1>&2
    goto :fail
)

echo == 転送 %REV% ^> %PVE_HOST% CT%CTID%:%DEST%
rem git archive なので作業ツリーの中途半端な状態は混ざらない。.env や data は送らない。
git archive --format=tar HEAD | ssh "%PVE_HOST%" "pct exec %CTID% -- tar -x -C %DEST%"
if errorlevel 1 goto :fail

rem 運用スクリプトは CT 内の /usr/local/bin にも置いておく。中で作業するとき用。
ssh "%PVE_HOST%" "pct exec %CTID% -- install -m 755 %DEST%/scripts/yomiage-ctl.sh /usr/local/bin/yomiage"
if errorlevel 1 goto :fail

echo == ビルドと再起動 %COMPOSE_FILE%
rem tts-bot と music-bot は同じイメージから作るので、両方まとめて再ビルドする。
rem voicevox は変更が無ければキャッシュがそのまま使われる。
ssh "%PVE_HOST%" "pct exec %CTID% -- sh -c 'cd %DEST% && docker compose -f %COMPOSE_FILE% up -d --build'"
if errorlevel 1 goto :fail

echo == 状態
ssh "%PVE_HOST%" "pct exec %CTID% -- sh -c 'cd %DEST% && docker compose -f %COMPOSE_FILE% ps'"
if errorlevel 1 goto :fail

echo.
echo ログ: ssh %PVE_HOST% "pct exec %CTID% -- docker logs -f yomiage-tts"
echo       ssh %PVE_HOST% "pct exec %CTID% -- docker logs -f yomiage-music"

popd
exit /b 0

:fail
popd
exit /b 1