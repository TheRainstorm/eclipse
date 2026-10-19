#!/usr/bin/env python3
"""Rebuild the locked dependency inventory and bundled license notices (no private paths)."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--metadata', type=Path, help='Existing cargo metadata JSON; otherwise run cargo metadata --locked')
    args = parser.parse_args()
    metadata = json.loads(args.metadata.read_text() if args.metadata else subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=ROOT, text=True))
    packages = sorted((p for p in metadata['packages'] if p['id'] not in metadata['workspace_members']),
                      key=lambda p: (p['name'], p['version']))
    output = ['uu-client third-party notices',
              'Generated from Cargo.lock, including optional, build, and other-platform dependencies.',
              'This is a source inventory, not a claim that every component is linked into every binary.',
              'Third-party licenses remain independent of the project MIT OR Apache-2.0 license.',
              'See docs/licensing-and-compatibility.md for native runtime, font, and patent considerations.', '']
    texts = {}
    inventory = ['# 锁定依赖与许可证', '',
                 '由 `python3 tools/licenses/generate.py` 根据 Cargo.lock 生成；包括可选、构建和其它平台依赖。',
                 '这不是某一二进制的链接清单，也不能替代对新增代码来源的人工检查。完整随包许可文本见 [THIRD-PARTY-NOTICES.txt](../THIRD-PARTY-NOTICES.txt)。', '',
                 '| 组件 | 版本 | 上游许可证表达式 |', '|---|---|---|']
    missing = []
    for package in packages:
        name, version = package['name'], package['version']
        license_expr = package.get('license')
        if not license_expr:
            raise ValueError(f'License metadata missing: {name} {version}')
        source = package.get('repository') or f'https://crates.io/crates/{name}/{version}'
        output += [f'## {name} {version}', f'SPDX: {license_expr}', f'Upstream: {source}',
                   'Authors: ' + '; '.join(package.get('authors', []))]
        inventory.append(f'| [{name}]({source}) | {version} | `{license_expr}` |')
        root = Path(package['manifest_path']).parent
        files = sorted(f for f in root.rglob('*') if f.is_file() and (
            f.name.lower().startswith(('license', 'licence', 'copying', 'notice', 'copyright'))
            or f.name in ('UFL.txt', 'OFL.txt', 'Hack-Regular.txt')) and f.stat().st_size < 2_000_000)
        for file in files:
            value = file.read_text(encoding='utf-8', errors='replace').strip()
            digest = hashlib.sha256(value.encode()).hexdigest()
            texts.setdefault(digest, value)
            output.append(f'License file: {file.relative_to(root).as_posix()} -> text {digest}')
        if not files:
            missing.append(f'{name} {version}')
            output.append('No separate license file in the crate archive; see the SPDX standard texts and upstream notices.')
        output.append('')
    for file in sorted((ROOT / 'licenses' / 'standard').glob('*.txt')):
        value = file.read_text().strip()
        digest = hashlib.sha256(value.encode()).hexdigest()
        texts.setdefault(digest, value)
        output.append(f'Standard license text: {file.name} -> text {digest}')
    for file in sorted((ROOT / 'licenses' / 'third-party').glob('*.txt')):
        value = file.read_text().strip()
        digest = hashlib.sha256(value.encode()).hexdigest()
        texts.setdefault(digest, value)
        output.append(f'Additional component notice: {file.name} -> text {digest}')
    output += ['', 'Crates without a separate license file in the published archive:', ', '.join(missing), '']
    for digest, value in sorted(texts.items()):
        output += [f'## License text {digest}', value, '']
    (ROOT / 'THIRD-PARTY-NOTICES.txt').write_text('\n'.join(output), encoding='utf-8')
    (ROOT / 'docs' / 'dependency-licenses.md').write_text('\n'.join(inventory) + '\n', encoding='utf-8')
    print(json.dumps({'packages': len(packages), 'distinct_license_texts': len(texts),
                      'crates_without_separate_license_file': len(missing)}))


if __name__ == '__main__':
    main()
