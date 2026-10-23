#!/usr/bin/env python3
"""Test Intel VPL ABI and lifecycle on an isolated fake runtime, without a GPU."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe',type=Path,required=True)
    parser.add_argument("--cli",type=Path,help="Also exercise native AV1 SDP/TLS/RTP with emulated codecs")
    args=parser.parse_args();probe=args.probe.resolve();root=Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix='eclipse-vpl-contract-') as directory:
        library=Path(directory)/'libvpl.so.2'
        subprocess.run(['cc','-shared','-fPIC','-DMFX_DEPRECATED_OFF','-I'+str(root/'vendor/libvpl-api/include'),
            str(root/'tools/native/vpl_runtime_fixture.c'),'-o',str(library)],check=True)
        env=os.environ.copy();env.update(LD_LIBRARY_PATH=directory,ECLIPSE_VPL_CONTRACT='1')
        for case in ('valid','low_power_unavailable','low_power_changed','software','fps','init_fps','codec','mode','partial','timestamp','buffer','decode_alignment','decode_allocation','decode_software','decode_size','decode_pool','decode_partial','decode_timestamp','decode_surface'):
            env['ECLIPSE_VPL_CONTRACT_CASE']=case
            result=subprocess.run([str(probe)],env=env,capture_output=True,text=True,timeout=15)
            if result.returncode:raise RuntimeError(case+': '+result.stdout[-2000:]+' / '+result.stderr[-2000:])
            report=json.loads(result.stdout.strip());assert report['passed'] and report['hardware_used'] is False,report
            print(json.dumps(report))

        if args.cli:
            import time
            cli=args.cli.resolve();invitation=Path(directory)/'generated.eclipse-invite.json';report=Path(directory)/'report.json'
            env['ECLIPSE_VPL_CONTRACT_CASE']='native_av1'
            # Configuration, sockets, account state and signals are all owned by
            # this fixture. No user desktop or input device is opened.
            env={key:value for key,value in env.items() if not key.startswith('UU_')}
            env.update(XDG_DATA_HOME=directory+'/data',XDG_STATE_HOME=directory+'/state',UU_DECODER='auto')
            log=open(Path(directory)/'host.log','w')
            host=subprocess.Popen([str(cli),'native-host','--synthetic','--synthetic-width','128','--synthetic-height','128',
                '--codec','av1','--encoder','hardware','--invitation-file',str(invitation),'--max-seconds','20'],env=env,stdout=log,stderr=log)
            try:
                deadline=time.monotonic()+10
                while not invitation.exists():
                    if host.poll() is not None or time.monotonic()>deadline:raise RuntimeError('Fixture host did not publish invitation')
                    time.sleep(.02)
                received=subprocess.run([str(cli),'native-probe','--invitation-file',str(invitation),'--frames','60','--timeout-seconds','15','--report',str(report)],env=env,capture_output=True,text=True,timeout=20)
                if received.returncode:raise RuntimeError('Native AV1 fixture: '+received.stderr[-2000:]+' / '+received.stdout[-2000:])
                native=json.loads(report.read_text());assert native['completed'] and native['final']['decoded']>=60 and native['final']['errors']==0,native
                assert native['final']['codec']=='video/AV1',native
                host.wait(timeout=3)
                assert host.returncode==0,'Native host did not close cleanly'
                print(json.dumps(dict(native_av1=True,encrypted_transport=True,emulated_runtime=True,hardware_validation=False,decoded=native['final']['decoded'])))
            finally:
                if host.poll() is None:host.terminate()
                try:host.wait(timeout=3)
                except subprocess.TimeoutExpired:host.kill();host.wait()
                log.close()

if __name__=='__main__':main()
