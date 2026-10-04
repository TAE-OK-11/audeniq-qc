#!/usr/bin/env python3
"""Compare verified normalization, including output decode and PCM equality."""
import argparse, datetime, hashlib, json, pathlib, platform, shutil, statistics, subprocess, tempfile
from benchmark import cpu_identity

def main():
    p=argparse.ArgumentParser()
    p.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/audeniq-qc'))
    p.add_argument('--seconds',type=int,default=60)
    p.add_argument('--repeats',type=int,default=3)
    p.add_argument('--output',type=pathlib.Path,required=True)
    a=p.parse_args();binary=a.binary.resolve()
    assert a.seconds>0 and a.repeats>=3
    with tempfile.TemporaryDirectory(prefix='audeniq-normalize-') as td:
        root=pathlib.Path(td);wav=root/'source.wav'
        subprocess.run(['ffmpeg','-v','error','-f','lavfi','-i',f'aevalsrc=0.4*sin(2*PI*997*t)+0.05*sin(2*PI*13001*t)|0.3*sin(2*PI*437*t)+0.04*sin(2*PI*9011*t):s=48000:d={a.seconds}','-c:a','pcm_s24le',str(wav)],check=True)
        def oracle(path):
            return ['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(path),'-map','0:a:0','-c:a','pcm_s32le','-f','hash','-hash','sha256','-']
        expected=subprocess.check_output(oracle(wav)).decode().strip().split('=')[1]
        rows=[]
        for codec,ext in [('pcm_s24le','wav'),('flac','flac'),('alac','m4a')]:
            src=wav if ext=='wav' else root/f'source.{ext}'
            if ext!='wav':subprocess.run(['ffmpeg','-v','error','-i',str(wav),'-c:a',codec,str(src)],check=True)
            outputs={name:root/f'{name}.flac' for name in ['native','ffmpeg']}
            commands={
                'native':[str(binary),'convert',str(src),str(outputs['native'])],
                'ffmpeg':['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(src),'-map','0:a:0','-map_metadata','-1','-c:a','flac','-threads','1','-compression_level','5',str(outputs['ffmpeg']),'-map','0:a:0','-c:a','pcm_s32le','-f','hash','-hash','sha256','-']}
            runs={name:[] for name in commands};sizes={};time_bin=shutil.which('time') or '/usr/bin/time'
            for rep in range(-1,a.repeats):
                for name in (['native','ffmpeg'] if rep%2 else ['ffmpeg','native']):
                    out=outputs[name];out.unlink(missing_ok=True);metrics=root/'time.txt'
                    result=subprocess.run([time_bin,'-f','%e %U %S %M','-o',str(metrics),*commands[name]],capture_output=True,check=True,timeout=120)
                    wall,user,system,rss=map(float,metrics.read_text().split());sizes[name]=out.stat().st_size
                    if name=='native':assert json.loads(result.stdout)['pcm_sha256']==expected
                    else:
                        assert result.stdout.decode().strip().split('=')[1]==expected
                        # AUDENIQ's verification is a second child; use max child RSS,
                        # sum CPU/wall time (process startup included).
                        verified=subprocess.run([time_bin,'-f','%e %U %S %M','-o',str(metrics),*oracle(out)],capture_output=True,check=True,timeout=120)
                        assert verified.stdout.decode().strip().split('=')[1]==expected
                        w,u,s,m=map(float,metrics.read_text().split());wall+=w;user+=u;system+=s;rss=max(rss,m)
                    # Independent FFmpeg verification for native is outside timing.
                    if name=='native':assert subprocess.check_output(oracle(out)).decode().strip().split('=')[1]==expected
                    if rep>=0:runs[name].append({'wall_s':wall,'user_s':user,'system_s':system,'cpu_s':user+system,'peak_rss_kib':int(rss)})
            medians={name:{key:statistics.median(r[key] for r in values) for key in values[0]} for name,values in runs.items()}
            row={'codec':codec,'source_bytes':src.stat().st_size,'fixture_sha256':hashlib.sha256(src.read_bytes()).hexdigest(),'pcm_sha256':expected,'commands':commands,'ffmpeg_verification_command':oracle(outputs['ffmpeg']),'output_bytes':sizes,'runs':runs,'median':medians,'ffmpeg_div_native':{key:medians['ffmpeg'][key]/medians['native'][key] for key in ['wall_s','cpu_s','peak_rss_kib']}}
            rows.append(row);print(codec,json.dumps(medians),sizes,flush=True)
        report={'date_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'host_cpu':cpu_identity(),'arch':platform.machine(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'ffmpeg_version':subprocess.check_output(['ffmpeg','-version'],text=True).splitlines()[0],'seconds':a.seconds,'sample_rate':48000,'channels':2,'bits':24,'repeats':a.repeats,'qualification':'Synthetic tones, warm cache. Both source decode/hash/FLAC encode and output decode/hash are timed. Native includes fsync and no-clobber publication. FFmpeg excludes separate probe and backend parsing overhead. Adaptive LPC/Rice or verified FLAC frame-copy and FFmpeg LPC level 5 produce different sizes; report sizes rather than claiming equal compression. Subsystem measurements, not end-to-end AUDENIQ. Host identity is reported; do not generalize to other machines.','results':rows}
        a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n')
if __name__=='__main__':main()
