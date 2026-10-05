import os,sys,random,struct,subprocess,collections
old,new,n=sys.argv[1],sys.argv[2],int(sys.argv[3]); files=sys.argv[4:]
T=os.environ.get('T','/tmp/jpgmut'); os.makedirs(T,exist_ok=True)
def segments(d):
    # returns list of [marker, body, entropy] ; entropy = bytes after SOS header up to next non-RST marker
    out=[];p=2
    while p<len(d):
        if d[p]!=0xff: break
        m=d[p+1]; p+=2
        if m in (0xd8,0xd9) or 0xd0<=m<=0xd7: out.append([m,b'',b'']); continue
        l=struct.unpack('>H',d[p:p+2])[0]; body=d[p+2:p+l]; p+=l; ent=b''
        if m==0xda:
            q=p
            while q<len(d)-1 and not (d[q]==0xff and d[q+1] not in (0,) and not (0xd0<=d[q+1]<=0xd7)): q+=1
            ent=d[p:q]; p=q
        out.append([m,body,ent])
    return out
def build(segs):
    o=bytearray(b'\xff\xd8')
    for m,b,e in segs:
        if m==0xd8: continue
        o+=bytes([0xff,m])
        if m==0xd9 or 0xd0<=m<=0xd7: continue
        o+=struct.pack('>H',len(b)+2)+b+e
    return bytes(o)
def run(b,p):
    r=subprocess.run([b,'image-probe',p],capture_output=True,timeout=60); return (r.returncode,r.stdout)
rng=random.Random(7); stats=collections.Counter(); bad=[]
def setbyte(b,i,v):
    b=bytearray(b)
    if i<len(b): b[i]=v&255
    return bytes(b)
for f in files:
    base=segments(open(f,'rb').read())
    for k in range(n):
        segs=[list(s) for s in base]; op=rng.randrange(12)
        sof=[i for i,s in enumerate(segs) if 0xc0<=s[0]<=0xcf and s[0] not in (0xc4,0xc8,0xcc)]
        sos=[i for i,s in enumerate(segs) if s[0]==0xda]
        if op==0 and sof:  # SOF fields
            b=segs[sof[0]][1]; i=rng.randrange(len(b))
            segs[sof[0]][1]=setbyte(b,i,rng.choice([0,1,2,3,4,0x11,0x22,0x12,0x21,0x41,0x14,0x13,0x31,0x44,8,12,16,255,rng.randrange(256)]))
        elif op==1 and sof:  # SOF type
            segs[sof[0]][0]=rng.choice([0xc0,0xc1,0xc2,0xc3,0xc5,0xc9,0xcd])
        elif op==2 and sos:  # SOS params
            j=rng.choice(sos); b=segs[j][1]; i=rng.randrange(len(b))
            segs[j][1]=setbyte(b,i,rng.choice([0,1,2,5,63,64,0x10,0x01,0x21,0x32,0x11,0xe,rng.randrange(256)]))
        elif op==3:  # DHT/DQT body byte
            ts=[i for i,s in enumerate(segs) if s[0] in (0xc4,0xdb)]
            if ts:
                j=rng.choice(ts); b=segs[j][1]; i=rng.randrange(min(len(b),40))
                segs[j][1]=setbyte(b,i,rng.choice([0,1,2,3,0x10,0x11,0x13,255,rng.randrange(256)]))
        elif op==4 and sos:  # entropy flips
            j=rng.choice(sos); e=bytearray(segs[j][2])
            for _ in range(rng.randint(1,3)):
                if e: e[rng.randrange(len(e))]^=1<<rng.randrange(8)
            segs[j][2]=bytes(e).replace(b'\xff\x00\xff',b'\xff\x00\x00')
        elif op==5 and sos:  # truncate entropy (then markers follow)
            j=rng.choice(sos); e=segs[j][2]; c=rng.randrange(len(e)+1); e=e[:c]
            if e.endswith(b'\xff'): e=e[:-1]
            segs[j][2]=e
        elif op==6:  # DRI insert / change
            v=rng.choice([0,1,2,3,7,1000])
            segs.insert(1,[0xdd,struct.pack('>H',v)[:rng.choice([2,2,2,1])],b''])
        elif op==7:  # remove EOI or a segment
            j=rng.randrange(1,len(segs)); del segs[j]
        elif op==8:  # duplicate a segment
            j=rng.randrange(1,len(segs)); segs.insert(j,list(segs[j]))
        elif op==9:  # APP14 Adobe / JFIF changes
            segs=[s for s in segs if not (s[0]==0xe0)] if rng.random()<.5 else segs
            segs.insert(1,[0xee,b'Adobe\0'+bytes([0,100,0,0,0,rng.choice([0,1,2,3])]),b''])
        elif op==10 and sos:  # stray RST / garbage between segments
            j=rng.choice(sos)+1
            segs.insert(j,[rng.choice([0xd0,0xd1,0xd3,0xd7,0xdc,0xfe]),b'' ,b''] if rng.random()<.5 else [0xfe,b'hi',b''])
        else:  # component ids
            if sof:
                b=bytearray(segs[sof[0]][1])
                if len(b)>=6+3*b[5]:
                    for c in range(b[5]): b[6+3*c]=rng.choice([1,2,3,82,71,66,34,35,c+1])
                    segs[sof[0]][1]=bytes(b)
        data=build(segs)
        if op==7 and rng.random()<.3: data=data+b'garbage'
        p=f'{T}/m.jpg'; open(p,'wb').write(data)
        a,bb=run(old,p),run(new,p)
        key=('ok' if a[0]==0 else ('panic' if a[0]<0 else 'err'))+('' if a==bb else '-DIFF'); stats[key]+=1
        if a!=bb: bad.append((f'{os.path.basename(f)}#{k}op{op}',a,bb)); open(f'{T}/diff{len(bad)}.jpg','wb').write(data)
print(dict(stats)); print("non-panic diffs:", sum(1 for _,a,b in bad if a[0]>=0))
for l,a,b in [x for x in bad if x[1][0]>=0][:40]: print('DIFF',l,'old',a[0],a[1][:60],'| new',b[0],b[1][:60])
