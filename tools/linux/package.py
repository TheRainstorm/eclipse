#!/usr/bin/env python3
"""Build the native Linux client with VAAPI and write a revision/integrity manifest."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']

def main():
    sources = ['crates', 'vendor', 'assets', 'Cargo.toml', 'Cargo.lock', 'tools/linux', 'tools/licenses', 'licenses', *LICENSES]
    subprocess.run(['git', 'diff', '--exit-code', 'HEAD', '--', *sources], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', '--', *sources], cwd=ROOT, text=True)
    if untracked.strip(): raise RuntimeError('Commit new source files before packaging')
    subprocess.run(['cargo', 'build', '--release', '-p', 'uu-gui', '-p', 'uu-cli', '--features', 'vaapi', '--locked'], cwd=ROOT, check=True)
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    dest = ROOT / 'artifacts' / 'packages' / f'uu-client-linux-{platform.machine()}-{revision[:12]}'
    dest.mkdir(parents=True, exist_ok=True)
    for name in ['uu-gui', 'uu']: shutil.copy2(ROOT / 'target' / 'release' / name, dest / name)
    shutil.copy2(ROOT / 'tools/linux/install.py', dest / 'install.py')
    shutil.copy2(ROOT / 'assets/branding/eclipse-512.png', dest / 'eclipse.png')
    for name in LICENSES: shutil.copy2(ROOT / name, dest / name)
    (dest / 'README.txt').write_text('uu-client — Linux\n\n安装：python3 install.py\n启动：~/.local/bin/uu-client，或应用菜单 uu-client。\nCLI：~/.local/bin/uu-rust\n\n构建启用 VA-API（H264），不支持的GPU自动回退OpenH264。\n运行需要 libva、libgbm、libpulse、ALSA（桌面推荐 PipeWire 的 ALSA 插件）、OpenGL/EGL。\n登录/设置：$XDG_DATA_HOME/uu-remote 或 ~/.local/share/uu-remote。\n支持 Wayland/X11；Wayland 系统快捷键捕获取决于合成器支持 shortcuts-inhibit 协议。\n默认保留被控端原有显示布局，独占虚拟屏仅在显式选择时启用。\n\n快捷键：Ctrl+Alt+S 信息；F/X 全屏；Q 退出串流；M 静音；H 悬浮入口；Z 捕获。\n')
    files = {name: hashlib.sha256((dest / name).read_bytes()).hexdigest() for name in ['uu-gui', 'uu', 'install.py', 'README.txt', 'eclipse.png', *LICENSES]}
    manifest = {'source_commit': revision, 'built_at_utc': datetime.now(timezone.utc).isoformat(), 'platform': f'linux-{platform.machine()}', 'features': ['vaapi', 'opus', 'wayland', 'x11'], 'build_glibc': platform.libc_ver(), 'sha256': files}
    (dest / 'version.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
    archive = shutil.make_archive(str(dest), 'gztar', root_dir=dest.parent, base_dir=dest.name)
    print(json.dumps({'source_commit': revision, 'directory': str(dest), 'archive': archive}))

if __name__ == '__main__':
    main()
