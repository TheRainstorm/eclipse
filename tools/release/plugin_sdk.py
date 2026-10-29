"""Capture the exact shared SDK artifact selected by the Cargo build graph."""
import json
import subprocess
import sys
import re
from pathlib import Path


def contract(root,target,features=()):
    command=['cargo','metadata','--locked','--format-version','1','--filter-platform',target]
    if features:command+=['--features',','.join(features)]
    metadata=json.loads(subprocess.check_output(command,cwd=root,text=True))
    packages={package['id']:package for package in metadata['packages']}
    nodes={node['id']:node for node in metadata['resolve']['nodes']}
    sdk=next(key for key,value in packages.items() if value['name']=='uu-plugin-sdk')
    pending=[sdk];seen=set()
    while pending:
        key=pending.pop()
        if key in seen:continue
        seen.add(key)
        pending.extend(dependency['pkg'] for dependency in nodes[key]['deps']
                       if any(kind['kind'] is None for kind in dependency['dep_kinds']))
    return {packages[key]['name']+'@'+packages[key]['version']:nodes[key]['features'] for key in seen}


def verify_consumer(engine,consumer,target,features=()):
    expected=contract(engine,target,features);actual=contract(consumer,target,features)
    mismatches=[key for key in expected if actual.get(key)!=expected[key]]
    if mismatches:
        raise RuntimeError('consumer changes shared SDK dependency versions/features: '+', '.join(sorted(mismatches)))


def verify_windows_imports(extension,sdk):
    objdump='objdump' if sys.platform=='win32' else 'x86_64-w64-mingw32-objdump'
    exports=subprocess.check_output([objdump,'-p',str(sdk)],text=True)
    available=set(re.findall(r'^\s*\[\s*\d+\]\s+(\S+)\s*$',exports,re.MULTILINE))
    imports=subprocess.check_output([objdump,'-p',str(extension)],text=True)
    section=imports.split('DLL Name: '+sdk.name,1)
    if len(section)!=2:raise RuntimeError('extension does not import the shared application SDK')
    section=section[1].split('DLL Name:',1)[0].split('The Export Tables',1)[0]
    required=set(re.findall(r'^\s*[0-9a-f]+\s+\d+\s+(\S+)\s*$',section,re.MULTILINE))
    missing=required-available
    if not required or missing:
        raise RuntimeError('extension imports SDK symbols absent from the main package: '+', '.join(sorted(missing)[:8]))
    return len(required)


def build(root, target, packages, features=(),environment=None):
    command = ['cargo', 'build', '--release', '--locked', '--target', target,
               '--message-format=json-render-diagnostics']
    for package in packages:
        command += ['-p', package]
    if features:
        command += ['--features', ','.join(features)]
    artifact = metadata = None
    process = subprocess.Popen(command, cwd=root, text=True, stdout=subprocess.PIPE,env=environment)
    for line in process.stdout:
        message = json.loads(line)
        if message.get("reason")=="compiler-message" and message.get("message",{}).get("rendered"):
            sys.stderr.write(message["message"]["rendered"])
        if message.get('target', {}).get('name') == 'uu_plugin_sdk' and message['reason'] == 'compiler-artifact':
            candidates = [Path(name) for name in message['filenames']
                          if name.endswith(('.dll', '.so', '.dylib'))]
            if len(candidates) != 1:
                raise RuntimeError('shared SDK artifact is ambiguous')
            artifact = candidates[0]
        if message['reason'] == 'build-script-executed' and 'uu-plugin-sdk' in message['package_id']:
            metadata = Path(message['out_dir']) / 'sdk.json'
    if process.wait():
        raise RuntimeError('Cargo build failed')
    if artifact is None or metadata is None:
        raise RuntimeError('Cargo did not identify the shared SDK and its metadata')
    compiler_lib = Path(subprocess.check_output(
        ['rustc', '--target', target, '--print', 'target-libdir'], cwd=root, text=True).strip())
    runtime = sorted(compiler_lib.glob('std-*.dll') if 'windows' in target
                     else compiler_lib.glob('libstd-*.so'))
    if len(runtime) != 1:
        raise RuntimeError('compiler shared standard library is ambiguous')
    return artifact, runtime[0], json.loads(metadata.read_text())
