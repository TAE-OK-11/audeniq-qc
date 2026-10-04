#!/usr/bin/env python3
"""Integer corner cases across codec compression modes; deterministic oracle."""
import argparse, hashlib, json, math, pathlib, random, subprocess, tempfile, wave

def run(args):
    p=subprocess.run([str(x) for x in args],capture_output=True,timeout=90)
    assert p.returncode==0,(args,p.stderr.decode(errors='replace'))
    return p.stdout

def main():
    p=argparse.ArgumentParser();p.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/audeniq-qc'));p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();binary=a.binary.resolve();checks=0;rows=[]
    with tempfile.TemporaryDirectory(prefix='audeniq-codec-stress-') as td:
        root=pathlib.Path(td)
        for depth in [16,24]:
            for channels in [1,2]:
                for pattern in ['zero','constant','impulse','ramp','limits','random','identical','opposite']:
                    rng=random.Random(1729);scale=1<<(depth-1);data=bytearray()
                    for i in range(8193):
                        mono=round(.9*scale*math.sin(i*.17))
                        for ch in range(channels):
                            value={'zero':0,'constant':scale//3,'impulse':-scale if i%127==0 else 0,'ramp':((i*7919+ch*71)%(2*scale))-scale,'limits':(-scale if (i+ch)%2 else scale-1),'random':rng.randrange(-scale,scale),'identical':mono,'opposite':mono if ch==0 else -mono}[pattern]
                            data.extend(value.to_bytes(depth//8,'little',signed=True))
                    src=root/'source.wav'
                    with wave.open(str(src),'wb') as f:f.setnchannels(channels);f.setsampwidth(depth//8);f.setframerate(48000);f.writeframes(data)
                    if len(data)%2:
                        b=bytearray(src.read_bytes());b.extend(b'\0');b[4:8]=(len(b)-8).to_bytes(4,'little');src.write_bytes(b)
                    # Canonical left-aligned s32 hash is independently generated.
                    raw=bytearray()
                    for i in range(0,len(data),depth//8):
                        x=int.from_bytes(data[i:i+depth//8],'little',signed=True)<<(32-depth);raw.extend(x.to_bytes(4,'little',signed=True))
                    expected=hashlib.sha256(raw).hexdigest()
                    profiles=[('flac',level,['-c:a','flac','-compression_level',str(level)]) for level in [0,5,12]]+ [('wv',level,['-c:a','wavpack','-bits_per_raw_sample',str(depth),'-compression_level',str(level)]) for level in [0,3,8]]+ [('tta',0,['-c:a','tta']),('m4a',0,['-c:a','alac'])]
                    for ext,level,opts in profiles:
                        path=root/f'encoded.{ext}';run(['ffmpeg','-nostdin','-v','error','-y','-i',src,*opts,path])
                        n=json.loads(run([binary,'pcm-hash',path]));assert n['pcm_sha256']==expected,(depth,channels,pattern,ext,level,n);checks+=1
                    out=root/'native.flac';out.unlink(missing_ok=True);run([binary,'convert',src,out])
                    decoded=run(['ffmpeg','-v','error','-xerror','-i',out,'-c:a','pcm_s32le','-f','hash','-hash','sha256','-']).decode().strip().split('=')[1]
                    assert decoded==expected,(depth,channels,pattern,decoded,expected);checks+=1
                    rows.append({'depth':depth,'channels':channels,'pattern':pattern,'profiles':len(profiles),'pcm_exact':True,'native_flac_exact':True})
        report={'status':'passed','checks':checks,'source':'independent deterministic PCM integers + FFmpeg development oracle','sample_rate':48000,'frames_per_case':8193,'cases':rows}
        a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
if __name__=='__main__':main()
