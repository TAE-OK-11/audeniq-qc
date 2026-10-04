#!/usr/bin/env python3
"""Measure wall/user/system CPU and per-process peak RSS; compare retained work."""
import argparse,datetime,hashlib,json,pathlib,platform,shutil,statistics,subprocess,tempfile

def main():
    p=argparse.ArgumentParser();p.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/audeniq-qc'));p.add_argument('--seconds',type=int,default=240);p.add_argument('--repeats',type=int,default=5);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();binary=a.binary.resolve()
    assert a.seconds>0 and a.repeats>=3
    with tempfile.TemporaryDirectory(prefix='audeniq-qc-bench-') as td:
        root=pathlib.Path(td);wav=root/'master.wav'
        subprocess.run(['ffmpeg','-v','error','-f','lavfi','-i',f'aevalsrc=0.4*sin(2*PI*997*t)+0.05*sin(2*PI*13001*t)|0.3*sin(2*PI*437*t)+0.04*sin(2*PI*9011*t):s=48000:d={a.seconds}','-c:a','pcm_s24le',str(wav)],check=True)
        rows=[]
        for codec,ext in [('pcm_s24le','wav'),('flac','flac'),('alac','m4a')]:
            src=wav if ext=='wav' else root/f'master.{ext}'
            if ext!='wav':subprocess.run(['ffmpeg','-v','error','-i',str(wav),'-c:a',codec,str(src)],check=True)
            commands={
              'native':[str(binary),'analyze',str(src)],
              'native_scalar':[str(binary),'analyze',str(src),'--scalar'],
              'ffmpeg':['ffmpeg','-nostdin','-hide_banner','-nostats','-threads','1','-i',str(src),'-map','0:a:0','-af','ebur128=peak=true:framelog=quiet','-f','null','-','-map','0:a:0','-c:a','pcm_s32le','-f','hash','-hash','sha256','-']}
            runs={k:[] for k in commands}
            # One unrecorded warm-up per implementation, then rotate run order.
            for cmd in commands.values():subprocess.run(cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
            order=list(commands)
            for rep in range(a.repeats):
                for name in order[rep%len(order):]+order[:rep%len(order)]:
                    metrics=root/'time.txt';cmd=[shutil.which('time') or '/usr/bin/time','-f','%e %U %S %M','-o',str(metrics),*commands[name]]
                    subprocess.run(cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True,timeout=120)
                    wall,user,system,rss=map(float,metrics.read_text().split());runs[name].append({'wall_s':wall,'user_s':user,'system_s':system,'cpu_s':user+system,'peak_rss_kib':int(rss)})
            medians={name:{key:statistics.median(r[key] for r in values) for key in values[0]} for name,values in runs.items()}
            ratio={key:medians['ffmpeg'][key]/medians['native'][key] for key in ['wall_s','cpu_s','peak_rss_kib']}
            rows.append({'codec':codec,'file_bytes':src.stat().st_size,'fixture_sha256':hashlib.sha256(src.read_bytes()).hexdigest(),'commands':commands,'runs':runs,'median':medians,'ffmpeg_div_native':ratio})
        cpu=next((line.split(':',1)[1].strip() for line in pathlib.Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name')),platform.processor())
        report={'date_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'host_cpu':cpu,'arch':platform.machine(),'ffmpeg_version':subprocess.check_output(['ffmpeg','-version'],text=True).splitlines()[0],'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'seconds':a.seconds,'sample_rate':48000,'channels':2,'bits':24,'repeats':a.repeats,'qualification':'synthetic tone mixture, warm page cache; not representative production corpus or EPYC/Arm results','comparison':'Both decode once and measure EBU LUFS/true peak + s32le SHA256. Native additionally measures clipping, silence, zero crossings and block energies. FFmpeg baseline excludes probe/startup for FFprobe and AUDENIQ Rust PCM meter; therefore these are conservative subsystem measurements, not an end-to-end AUDENIQ speedup. Fingerprint tap disabled for both. True-peak algorithms differ; see qualification tolerances.','results':rows}
        a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n')
        for r in rows:print(r['codec'],json.dumps(r['median']),json.dumps(r['ffmpeg_div_native']))
if __name__=='__main__':main()
