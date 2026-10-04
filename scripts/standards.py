#!/usr/bin/env python3
"""Synthesize EBU Tech 3341 test descriptions; do not redistribute EBU media.

Subset only: integrated loudness cases 1..5 and true peak 15..19, extended to
44.1/48/96kHz. Not a certification or replacement for the complete EBU set.
"""
import argparse,json,math,pathlib,subprocess,tempfile,wave

def signal(path,rate,sections,frequency=1000,phase=0,fade=False,amplitude=False):
    with wave.open(str(path),'wb') as f:
        f.setnchannels(2);f.setsampwidth(3);f.setframerate(rate)
        cursor=0
        for duration,level in sections:
            count=round(duration*rate);scale=level if amplitude else 10**(level/20)
            data=bytearray()
            for i in range(count):
                gain=min(1,i/(rate*.01),(count-1-i)/(rate*.01)) if fade else 1
                v=round(scale*gain*math.sin(2*math.pi*frequency*(cursor+i)/rate+phase)*(1<<23))
                assert -(1<<23)<=v<(1<<23),'fixture would clip'
                data+=v.to_bytes(3,'little',signed=True)*2
            f.writeframes(data);cursor+=count

def main():
    p=argparse.ArgumentParser();p.add_argument('--binary',type=pathlib.Path,default=pathlib.Path('target/release/audeniq-qc'));p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args();binary=a.binary.resolve();rows=[]
    with tempfile.TemporaryDirectory(prefix='audeniq-ebu-synth-') as td:
        root=pathlib.Path(td)
        loudness={1:[(20,-23)],2:[(20,-33)],3:[(10,-36),(60,-23),(10,-36)],4:[(10,-72),(10,-36),(60,-23),(10,-36),(10,-72)],5:[(20,-26),(20.1,-20),(20,-26)]}
        for case,sections in loudness.items():
            path=root/f'case{case}.wav';signal(path,48000,sections)
            n=json.loads(subprocess.check_output([str(binary),'analyze',str(path)]));expected=-33 if case==2 else -23;actual=n['integrated_lufs'];assert abs(actual-expected)<=.1,(case,actual,expected)
            rows.append({'test':case,'metric':'integrated_lufs','expected':expected,'actual':actual,'tolerance':.1,'passed':True})
        for rate in [44100,48000,96000]:
            for case,divisor,phase,amplitude in [(15,4,0,.5),(16,4,math.pi/4,.5),(17,6,math.pi/3,.5),(18,8,3*math.pi/8,.5),(19,4,math.pi/4,1.41)]:
                path=root/f'case{case}-{rate}.wav';signal(path,rate,[(2,amplitude)],frequency=rate/divisor,phase=phase,fade=True,amplitude=True)
                n=json.loads(subprocess.check_output([str(binary),'analyze',str(path)]));expected=3 if case==19 else -6;actual=n['true_peak_dbtp'];assert expected-.4<=actual<=expected+.2,(case,rate,actual,expected)
                rows.append({'test':case,'sample_rate':rate,'metric':'true_peak_dbtp','expected':expected,'actual':actual,'tolerance_lower':-.4,'tolerance_upper':.2,'passed':True})
    report={'reference':'EBU Tech 3341 (November 2023), Table 1','source':'https://tech.ebu.ch/files/live/sites/tech/files/shared/tech/tech3341.pdf','test_material':'Independently synthesized from published signal descriptions, not original EBU ZIP. ZIP download returned HTTP 403 in development environment.','scope':'5 integrated loudness tests + 5 true peak tests at 3 rates; cases 6..14 and transient peak cases 20..23 are not covered. Only accepted integer mono/stereo formats are in scope.','certified':False,'status':'passed','results':rows}
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
if __name__=='__main__':main()
