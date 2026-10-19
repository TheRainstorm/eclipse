#!/usr/bin/env python3
"""Verify a release ZIP, upload over OpenSSH, and run the Windows installer."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import uuid
import zipfile

from package import OUTPUT, REQUIRED, sha256


def verify_package(path):
    with zipfile.ZipFile(path) as bundle:
        names = bundle.namelist()
        # Only flat, known files are allowed; reject traversal, duplicate names and extra executables.
        if len(names) != len(REQUIRED) + 1 or set(names) != set(REQUIRED) | {'version.json'}:
            raise ValueError('ZIP 文件清单不正确，请使用 tools/windows/package.py 生成的包')
        manifest = json.loads(bundle.read('version.json'))
        if not re.fullmatch(r'[0-9a-f]{40}', manifest.get('source_commit', '')):
            raise ValueError('version.json 缺少有效提交号')
        if set(manifest.get('sha256', {})) != set(REQUIRED):
            raise ValueError('version.json 校验清单不完整')
        for name in REQUIRED:
            with bundle.open(name) as stream:
                actual = hashlib.file_digest(stream, 'sha256').hexdigest()
            if actual != manifest['sha256'][name]:
                raise ValueError(f'文件校验失败：{name}')
    return manifest


def latest_package():
    pointer = OUTPUT / 'windows-latest.json'
    if not pointer.is_file():
        raise ValueError('找不到最新 Windows 包，请先执行 python3 tools/windows/package.py')
    record = json.loads(pointer.read_text(encoding='utf-8'))
    name = record['archive']
    if not isinstance(name, str) or Path(name).name != name:
        raise ValueError('windows-latest.json 的文件名无效')
    path = OUTPUT / name
    if sha256(path) != record['sha256']:
        raise ValueError('最新 ZIP 校验失败，请重新打包')
    return path


def ps_literal(value):
    return "'" + value.replace("'", "''") + "'"


def install_script(upload_name, digest, revision, install_dir, no_shortcut):
    # The ASCII upload basename is relative to the SSH account's default working directory.
    # PowerShell and scp inherit that same directory; no username/path interpolation is needed.
    return f"""
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$uploaded = Join-Path (Get-Location).Path {ps_literal(upload_name)}
$stage = Join-Path $env:TEMP {ps_literal(upload_name + '-unpack')}
try {{
    if ((Get-FileHash -LiteralPath $uploaded -Algorithm SHA256).Hash -ne {ps_literal(digest)}) {{
        throw 'Uploaded ZIP hash mismatch'
    }}
    Expand-Archive -LiteralPath $uploaded -DestinationPath $stage
    $manifest = Get-Content -LiteralPath (Join-Path $stage 'version.json') -Raw | ConvertFrom-Json
    if ($manifest.source_commit -ne {ps_literal(revision)}) {{ throw 'Source commit mismatch' }}
    & (Join-Path $stage 'install.ps1') -SourceDir $stage -InstallDir {ps_literal(install_dir)} {'-NoShortcut' if no_shortcut else ''}
}} catch {{
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}} finally {{
    Remove-Item -LiteralPath $uploaded -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
}}
"""


def sync_package(args):
    path = (args.package or latest_package()).resolve()
    manifest = verify_package(path)
    digest = sha256(path)
    if not re.fullmatch(r'[A-Za-z0-9_.-]+', args.host) or args.host.startswith('-'):
        raise ValueError('--host 请填写 IPv4 地址或 SSH 主机别名')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+', args.user) or args.user.startswith('-'):
        raise ValueError('--user 请填写 Windows SSH 登录用户名（字母、数字、点、横线或下划线）')
    if not 1 <= args.port <= 65535:
        raise ValueError('--port 必须在 1–65535 之间')
    if not re.match(r'^[A-Za-z]:[\\/]', args.install_dir) or any(c in args.install_dir for c in '\r\n\x00'):
        raise ValueError('--install-dir 必须是 Windows 盘符绝对路径')
    target = f'{args.user}@{args.host}'
    upload_name = f'uu-remote-upload-{uuid.uuid4().hex}.zip'
    options = ['-o', 'ConnectTimeout=10', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3']
    if args.identity:
        options += ['-i', str(args.identity.expanduser().resolve())]
    scp = ['scp', '-O', '-P', str(args.port), *options, str(path), f'{target}:{upload_name}']
    script = install_script(upload_name, digest, manifest['source_commit'], args.install_dir, args.no_shortcut)
    encoded = base64.b64encode(script.encode('utf-16-le')).decode('ascii')
    ssh = ['ssh', '-p', str(args.port), *options, target, 'powershell.exe', '-NoProfile',
           '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', encoded]
    print(f"版本：{manifest['source_commit']}\n产物：{path}\n目标：{target}:{args.port} → {args.install_dir}", flush=True)
    if args.dry_run:
        print('预演完成，本地 ZIP 与逐文件 SHA256 均已通过；未连接远端。')
        print(shlex.join(scp))
        print('SSH 将校验上传 ZIP、解压并执行 install.ps1；PowerShell 内容：\n' + script)
        return
    for tool in ('scp', 'ssh'):
        if not shutil.which(tool):
            raise RuntimeError(f'缺少 {tool}，请安装 openssh-client')
    subprocess.run(scp, check=True)
    subprocess.run(ssh, check=True)
    print('同步完成。请关闭旧版窗口，再从桌面 UU Remote Rust 重新打开。')


def main():
    parser = argparse.ArgumentParser(description='通过 Windows OpenSSH 同步并安装 Windows ZIP（Python 3.11+）')
    parser.add_argument('--host', required=True, help='Windows IPv4 地址或 SSH 主机别名')
    parser.add_argument('--user', default='yfy', help='SSH 用户名（默认 yfy）')
    parser.add_argument('--port', type=int, default=2202, help='SSH 端口（默认 2202）')
    parser.add_argument('--identity', type=Path, help='SSH 私钥文件，也可使用 ssh-agent 或 ~/.ssh/config')
    parser.add_argument('--package', type=Path, help='指定 ZIP；默认读取 artifacts/packages/windows-latest.json')
    parser.add_argument('--install-dir', default=r'D:\SMB\uu-gui-demo', help='Windows 安装目录')
    parser.add_argument('--no-shortcut', action='store_true', help='不修改桌面或开始菜单快捷方式')
    parser.add_argument('--dry-run', action='store_true', help='只校验本地包、显示操作，不连接或写入远端')
    args = parser.parse_args()
    try:
        sync_package(args)
    except (OSError, ValueError, KeyError, RuntimeError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        print(f'同步失败：{error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
