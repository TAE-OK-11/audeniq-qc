#!/usr/bin/env python3
"""Deterministic FFmpeg oracle qualification. FFmpeg is dev-only, never runtime."""
import argparse, hashlib, json, math, pathlib, random, re, shutil, struct, subprocess, tempfile, wave

def run(args):
    p=subprocess.run([str(x) for x in args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
    if p.returncode: raise RuntimeError(f'{args}: {p.stderr.decode(errors="replace")}')
    return p

def oracle_hash(path):
    return run(['ffmpeg','-v','error','-xerror','-i',path,'-map','0:a:0','-c:a','pcm_s32le','-f','hash','-hash','sha256','-']).stdout.decode().strip().split('=')[1]

def native(binary,command,*paths,**kw):
    args=[binary,command,*paths]
    if kw.get('scalar'): args+=['--scalar']
    if kw.get('fingerprint'): args+=['--fingerprint']
    return json.loads(run(args).stdout)

def write_wave(path,rate,depth,channels,kind,duration=2):
    rng=random.Random(1729);count=round(rate*duration);data=bytearray();scale=1<<(depth-1)
    for i in range(count):
        t=i/rate
        for ch in range(channels):
            if kind=='silence':v=0
            elif kind=='noise':v=rng.randrange(-scale,scale)
            elif kind=='clip':v=(scale-1 if (i//11)%2 else -scale) if i%11<7 else 0
            elif kind=='gated':v=0 if t<0.4 or t>1.7 else round(scale*.4*math.sin(2*math.pi*(997+ch*337)*t))
            elif kind=='near_nyquist':v=round(scale*.9*min(1.0,t/.01,(duration-t)/.01)*math.sin(2*math.pi*rate*.24*t+math.pi/4))
            else:v=round(scale*(.4*math.sin(2*math.pi*(997+ch*337)*t)+.06*math.sin(2*math.pi*13001*t)))
            v=max(-scale,min(scale-1,v));data+=v.to_bytes(depth//8,'little',signed=True)
    with wave.open(str(path),'wb') as f:f.setnchannels(channels);f.setsampwidth(depth//8);f.setframerate(rate);f.writeframes(data)

def oracle_loudness(path):
    text=run(['ffmpeg','-hide_banner','-nostats','-i',path,'-af','ebur128=peak=true:framelog=quiet','-f','null','-']).stderr.decode()
    loud=re.findall(r'I:\s+([\-\w.]+) LUFS',text)[-1]
    peak=re.findall(r'Peak:\s+([\-\w.]+) dBFS',text)[-1]
    return float(loud),float(peak)

def rejected(binary,command,path,*extra):
    p=subprocess.run([str(binary),command,str(path),*[str(x) for x in extra]],capture_output=True,timeout=20)
    assert p.returncode==2,(path,p.returncode,p.stdout,p.stderr)
    error=json.loads(p.stderr);assert error['error'] in ('INVALID_INPUT','UNSUPPORTED','RESOURCE_LIMIT','IO','DEADLINE'),error

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/audeniq-qc'));ap.add_argument('--output',type=pathlib.Path);a=ap.parse_args();binary=a.binary.resolve()
    results=[];checks=0
    with tempfile.TemporaryDirectory(prefix='audeniq-qc-oracle-') as td:
        root=pathlib.Path(td)
        for rate,depth,channels in [(44100,16,1),(48000,24,2),(96000,24,2),(192000,16,2)]:
            src=root/f'{rate}-{depth}-{channels}.wav';write_wave(src,rate,depth,channels,'tone',1.2)
            expected=oracle_hash(src)
            formats=[('wav',['-c:a',f'pcm_s{depth}le']),('flac',['-c:a','flac','-sample_fmt','s16' if depth==16 else 's32']),('m4a',['-c:a','alac']),('aiff',['-c:a',f'pcm_s{depth}be']),('tta',['-c:a','tta']),('wv',['-c:a','wavpack','-bits_per_raw_sample',str(depth)]),('rf64.wav',['-c:a',f'pcm_s{depth}le','-rf64','always'])]
            for ext,opts in formats:
                path=root/f'{rate}-{depth}-{channels}.encoded.{ext}'
                run(['ffmpeg','-v','error','-i',src,*opts,'-y',path])
                report=native(binary,'pcm-hash',path);assert report['pcm_sha256']==expected,(path,report,expected);checks+=1
                assert report['frames']==round(rate*1.2);checks+=1
                analysis=native(binary,'analyze',path);assert analysis['pcm_sha256']==expected;checks+=1
                out=root/f'{path.name}.out.flac';converted=native(binary,'convert',path,out)
                assert converted['pcm_sha256']==expected and oracle_hash(out)==expected,(path,converted);checks+=1
                if ext=='flac':assert converted['encoder']=='verified-flac-frame-copy-v1';checks+=1
                assert converted['spec']['bits_per_sample']==depth and converted['spec']['sample_rate']==rate;checks+=1
                probe=native(binary,'probe',path);assert probe['streams'][0]['codec_type']=='audio';checks+=1
                results.append({'case':path.name,'pcm_exact':True,'roundtrip_exact':True})
        for kind in ['tone','silence','noise','clip','gated','near_nyquist']:
            src=root/f'{kind}.wav';write_wave(src,48000,24,2,kind)
            n=native(binary,'analyze',src);s=native(binary,'analyze',src,scalar=True)
            for key in ['clip_events','clipped_samples','silent_blocks','longest_silent_run','zero_crossing_rate','pcm_sha256','samples_per_channel','integrated_lufs']:
                assert n[key]==s[key],(kind,key,n[key],s[key]);checks+=1
            i,tp=oracle_loudness(src)
            if kind=='silence':assert n['integrated_lufs'] is None and n['true_peak_dbtp'] is None and n['silent_blocks']==40;checks+=1
            else:
                assert abs(n['integrated_lufs']-i)<=.11,(kind,n['integrated_lufs'],i);checks+=1
                if kind!='noise':assert abs(n['true_peak_dbtp']-tp)<=.5,(kind,n['true_peak_dbtp'],tp);checks+=1
            if kind=='clip':assert n['clip_events']>100 and n['clipped_samples']>1000;checks+=1
            results.append({'case':kind,'native_lufs':n['integrated_lufs'],'ffmpeg_lufs':i if math.isfinite(i) else None,'native_true_peak':n['true_peak_dbtp'],'ffmpeg_true_peak':tp if math.isfinite(tp) else None})
        src=root/'bad-source.wav';write_wave(src,48000,24,2,'tone')
        for ext,opts in [('wav',[]),('flac',['-c:a','flac']),('m4a',['-c:a','alac']),('tta',['-c:a','tta']),('wv',['-c:a','wavpack','-bits_per_raw_sample','24'])]:
            good=src if ext=='wav' else root/f'good.{ext}'
            if ext!='wav':run(['ffmpeg','-v','error','-i',src,*opts,'-y',good])
            b=good.read_bytes();bad=root/f'truncated.{ext}';bad.write_bytes(b[:len(b)*4//5]);rejected(binary,'analyze',bad);checks+=1
            output=root/f'bad.{ext}.flac';rejected(binary,'convert',bad,output);assert not output.exists();checks+=1
            assert not list(root.glob(f'.{output.name}.*.partial'));checks+=1
            if ext in ('tta','wv','flac'):
                bad=root/f'corrupt.{ext}';corrupt=bytearray(b);corrupt[len(b)//2]^=0x80;bad.write_bytes(corrupt);rejected(binary,'analyze',bad);checks+=1
        # Non-lossless disguised in an accepted M4A container must fail closed.
        lossy=root/'lossy.m4a';run(['ffmpeg','-v','error','-i',src,'-c:a','aac',lossy]);rejected(binary,'analyze',lossy);checks+=1
        good=root/'good.wv';b=good.read_bytes();hidden=bytearray(b);flags=struct.unpack_from('<I',hidden,24)[0];struct.pack_into('<I',hidden,24,flags|8)
        footer=b'APETAGEX'+struct.pack('<IIII',2000,len(hidden)+32,0,0)+b'\0'*8
        forged=root/'ape-hidden-hybrid.wv';forged.write_bytes(b+hidden+footer);rejected(binary,'analyze',forged);checks+=1
        unknown=root/'unknown-length.flac';b=bytearray((root/'good.flac').read_bytes());packed=int.from_bytes(b[18:26],'big');b[18:26]=(packed&~((1<<36)-1)).to_bytes(8,'big');unknown.write_bytes(b);rejected(binary,'analyze',unknown);checks+=1
        # Resampler basic cardinality, anti-aliasing and bounded retained windows.
        fp=native(binary,'analyze',src,fingerprint=True);assert len(fp['fingerprint_windows'][0]['samples'])==22050;checks+=1
        alias=root/'alias.wav';run(['ffmpeg','-v','error','-f','lavfi','-i','sine=frequency=15000:sample_rate=48000:duration=2','-ac','2','-c:a','pcm_s24le',alias]);fp=native(binary,'analyze',alias,fingerprint=True)
        mid=fp['fingerprint_windows'][0]['samples'][100:-100];rms=math.sqrt(sum(x*x for x in mid)/len(mid));assert rms<10,rms;checks+=1
        long=root/'long.wav';run(['ffmpeg','-v','error','-f','lavfi','-i','sine=frequency=437:sample_rate=48000:duration=121.235','-ac','2','-c:a','pcm_s24le',long])
        fused=native(binary,'analyze',long,fingerprint=True);fallback=native(binary,'fingerprint',long)
        assert fused['fingerprint_windows']==fallback['fingerprint_windows'];checks+=1
        assert fallback['fingerprint_windows']==native(binary,'fingerprint',long,scalar=True)['fingerprint_windows'];checks+=1
        windows=fallback['fingerprint_windows'];assert len(windows)==3 and all(len(w['samples'])==330750 for w in windows);checks+=1
        assert all(abs(w['start_secs']-expected)<1e-6 for w,expected in zip(windows,[0.0,45.6175,91.235]));checks+=1
        # Native cover decoding and metadata fallback.
        png=root/'cover.png';jpg=root/'cover.jpg'
        run(['ffmpeg','-v','error','-f','lavfi','-i','color=c=red:s=64x64','-frames:v','1','-threads','1',png]);run(['ffmpeg','-v','error','-i',png,'-frames:v','1','-threads','1',jpg])
        for cover in [png,jpg]:assert native(binary,'image-probe',cover)['streams'][0]['width']==64;checks+=1
        for ext,opts in [('flac',['-c:a','flac']),('m4a',['-c:a','alac']),('aiff',['-c:a','pcm_s24be']),('wav',['-c:a','pcm_s24le']),('wv',['-c:a','wavpack','-bits_per_raw_sample','24']),('tta',['-c:a','tta'])]:
            tagged=root/f'tagged.{ext}';run(['ffmpeg','-v','error','-i',src,'-metadata','comment=AUDENIQ oracle',*opts,tagged])
            assert native(binary,'tags',tagged)['format']['tags']['comment']=='AUDENIQ oracle',ext;checks+=1
            normalized=root/f'tagged-{ext}.normalized.flac';native(binary,'convert',tagged,normalized)
            assert native(binary,'tags',normalized)['format']['tags']=={},ext;checks+=1
    report={'checks':checks,'status':'passed','cases':results,'true_peak_tolerance_db':.5,'true_peak_comparison_exclusions':'Full-band unfiltered white noise: not band-limited to EBU 20kHz tolerance range. Difference recorded, not claimed bit-identical. Abrupt high-frequency tone replaced by 10ms fade as EBU specifies.','lufs_tolerance_lu':.11,'true_peak_certified':False}
    if a.output:a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))
if __name__=='__main__':main()
