#!/usr/bin/env python3
"""Test Intel VPL ABI and lifecycle on an isolated fake runtime, without a GPU."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe',type=Path,required=True)
    args=parser.parse_args();probe=args.probe.resolve();root=Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix='eclipse-vpl-contract-') as directory:
        library=Path(directory)/'libvpl.so.2'
        subprocess.run(['cc','-shared','-fPIC','-DMFX_DEPRECATED_OFF','-I'+str(root/'vendor/libvpl-api/include'),
            str(root/'tools/native/vpl_runtime_fixture.c'),'-o',str(library)],check=True)
        env=os.environ.copy();env.update(LD_LIBRARY_PATH=directory,ECLIPSE_VPL_CONTRACT='1')
        for case in ('valid','software','fps','init_fps','codec','mode','partial','timestamp','buffer'):
            env['ECLIPSE_VPL_CONTRACT_CASE']=case
            result=subprocess.run([str(probe)],env=env,capture_output=True,text=True,timeout=15)
            if result.returncode:raise RuntimeError(case+': '+result.stdout[-2000:]+' / '+result.stderr[-2000:])
            report=json.loads(result.stdout.strip());assert report['passed'] and report['hardware_used'] is False,report
            print(json.dumps(report))

if __name__=='__main__':main()
