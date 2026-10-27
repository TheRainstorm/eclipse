"""Verify the committed source tree and every pinned Git submodule before packaging."""
import subprocess


def revision(root, paths):
    status = subprocess.check_output(
        ['git', 'status', '--porcelain', '--ignore-submodules=none', '--untracked-files=all', '--', *paths],
        cwd=root, text=True)
    if status.strip():
        raise RuntimeError('Commit all package sources, including plugin changes, first:\n' + status)
    modules = subprocess.check_output(['git', 'submodule', 'status', '--recursive'], cwd=root, text=True)
    if any(line and not line.startswith(' ') for line in modules.splitlines()):
        raise RuntimeError('Initialize submodules at the pinned commits: git submodule update --init --recursive')
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
