import os,sys,random,struct,zlib,subprocess,collections
old,new,n=sys.argv[1],sys.argv[2],int(sys.argv[3]); files=sys.argv[4:]
T=os.environ.get('T','/tmp/pngmut'); os.makedirs(T,exist_ok=True)
def chunks(d):
    out=[];p=8
    while p+8<=len(d):
        l=struct.unpack('>I',d[p:p+4])[0]; t=d[p+4:p+8]; out.append([t,d[p+8:p+8+l]]); p+=12+l
    return out
def build(cs,crc=True,sig=b'\x89PNG\r\n\x1a\n'):
    o=bytearray(sig)
    for t,b in cs:
        o+=struct.pack('>I',len(b))+t+b+struct.pack('>I',zlib.crc32(t+b)&0xffffffff if crc else 0)
    return bytes(o)
def run(b,p):
    r=subprocess.run([b,'image-probe',p],capture_output=True,timeout=60); return (r.returncode,r.stdout)
rng=random.Random(99); stats=collections.Counter(); bad=[]
ANC=[b'gAMA',b'sRGB',b'pHYs',b'cHRM',b'sBIT',b'tRNS',b'iCCP',b'acTL',b'fcTL',b'bKGD',b'tEXt',b'zzZZ',b'ABCD',b'PLTE',b'IHDR',b'IEND',b'fdAT',b'eXIf']
def rand_body(t,cs):
    if t==b'iCCP':
        prof=os.urandom(rng.randrange(0,300)) if rng.random()<.5 else b'\0'*rng.randrange(0,5000)
        z=zlib.compress(prof); 
        if rng.random()<.3: z=z[:-1]+bytes([z[-1]^1])
        return b'prof'[:rng.randrange(0,5)]+b'\0'+bytes([rng.choice([0,0,1])])+z
    if t==b'fcTL':
        ih=cs[0][1]; w,h=struct.unpack('>II',ih[:8])
        fw=rng.choice([w,max(1,w//2),w+1,0]); fh=rng.choice([h,max(1,h//2),h+1])
        return struct.pack('>IIIIIHHBB',rng.choice([0,0,1]),fw,fh,rng.choice([0,0,1]),0,1,1,rng.choice([0,0,3]),rng.choice([0,0,2]))[:rng.choice([26,26,25])]
    if t==b'acTL': return struct.pack('>II',rng.choice([0,1,2]),0)[:rng.choice([8,8,7])]
    L={b'gAMA':4,b'sRGB':1,b'pHYs':9,b'cHRM':32,b'sBIT':rng.choice([1,2,3,4]),b'tRNS':rng.choice([0,1,2,6,3,256]),b'PLTE':rng.choice([0,3,6,48,768,769,10,771]),b'bKGD':2,b'IHDR':13}.get(t,rng.randrange(0,20))
    L=max(0,L+rng.choice([0,0,0,-1,1]))
    return bytes(rng.randrange(256) if t not in (b'sRGB',b'pHYs') else rng.randrange(4) for _ in range(L))
for f in files:
    base=chunks(open(f,'rb').read())
    for k in range(n):
        cs=[[t,bytes(b)] for t,b in base]; crc=True; sig=b'\x89PNG\r\n\x1a\n'; op=rng.randrange(10)
        idx=[i for i,(t,_) in enumerate(cs) if t==b'IDAT']
        if op==0:  # IHDR field
            ih=bytearray(cs[0][1]); i=rng.randrange(13)
            ih[i]=rng.choice([0,1,2,3,4,6,8,16,255,rng.randrange(256)]); cs[0][1]=bytes(ih)
        elif op==1:  # insert ancillary before first IDAT
            t=rng.choice(ANC); cs.insert(rng.randrange(1,idx[0]+1),[t,rand_body(t,cs)])
        elif op==2:  # mutate decompressed data and recompress
            raw=bytearray(zlib.decompress(b''.join(cs[i][1] for i in idx)))
            m=rng.randrange(4)
            if m==0 and raw: raw[rng.randrange(len(raw))]=rng.choice([5,4,0,255])
            elif m==1: raw=raw[:rng.randrange(len(raw)+1)]
            elif m==2: raw+=os.urandom(rng.randrange(1,50))
            else:
                if raw: raw[0]=rng.randrange(8)
            z=zlib.compress(bytes(raw),rng.choice([0,1,6,9]))
            mm=rng.randrange(5)
            if mm==1: z=z[:-rng.randrange(1,5)]
            elif mm==2: z=z[:-4]+bytes(4)
            elif mm==3: z=z+os.urandom(rng.randrange(1,10))
            for i in reversed(idx): del cs[i]
            parts=[z[:len(z)//2],z[len(z)//2:]] if rng.random()<.5 else [z]
            if rng.random()<.2: parts.insert(1,b'')
            for j,pp in enumerate(parts): cs.insert(idx[0]+j,[b'IDAT',pp])
        elif op==3:  # corrupt one CRC
            crc=False if rng.random()<.2 else True
            j=rng.randrange(len(cs)); t,b=cs[j]
            data=build(cs); 
            # flip CRC of chunk j
            p=8
            for q in range(j): p+=12+len(cs[q][1])
            data=bytearray(data); e=p+8+len(b); data[e]^=0xff
            path=f'{T}/m.png'; open(path,'wb').write(data); 
            a,bb=run(old,path),run(new,path); key=('ok' if a[0]==0 else 'err')+('' if a==bb else '-DIFF'); stats[key]+=1
            if a!=bb: bad.append((f'{os.path.basename(f)}#{k}crc{t}',a,bb)); open(f'{T}/diff{len(bad)}.png','wb').write(data)
            continue
        elif op==4:  # drop or duplicate a chunk
            j=rng.randrange(len(cs))
            if rng.random()<.5: del cs[j]
            else: cs.insert(j,list(cs[j]))
        elif op==5:  # trailing stuff after IDAT
            last=idx[-1]
            tail=rng.choice([[],[[b'IEND',b'']],[[b'tEXt',b'a\0b']],[[b'IDAT',b'xx']]])
            cs=cs[:last+1]+tail
            data=build(cs)
            cut=rng.choice([0,0,4,8,12])
            data=data[:len(data)-cut] if cut else data
            path=f'{T}/m.png'; open(path,'wb').write(data)
            a,bb=run(old,path),run(new,path); key=('ok' if a[0]==0 else 'err')+('' if a==bb else '-DIFF'); stats[key]+=1
            if a!=bb: bad.append((f'{os.path.basename(f)}#{k}tail',a,bb)); open(f'{T}/diff{len(bad)}.png','wb').write(data)
            continue
        elif op==6:  # PLTE changes for any image
            pl=[i for i,(t,_) in enumerate(cs) if t==b'PLTE']
            body=bytes(rng.randrange(256) for _ in range(rng.choice([0,3,30,768,769,770,6,7])))
            if pl: cs[pl[0]][1]=body
            else: cs.insert(1,[b'PLTE',body])
        elif op==7:  # APNG: fcTL + IDAT or fdAT
            ih=cs[0][1]; w,h=struct.unpack('>II',ih[:8])
            cs.insert(idx[0],[b'fcTL',struct.pack('>IIIIIHHBB',0,w,h,0,0,1,1,0,0)])
            if rng.random()<.5: cs.insert(1,[b'acTL',struct.pack('>II',rng.choice([0,1,2]),0)])
            if rng.random()<.4:
                ii=[i for i,(t,_) in enumerate(cs) if t==b'IDAT']
                for s,i in enumerate(ii): cs[i]=[b'fdAT',struct.pack('>I',s+1+rng.choice([0,0,1]))+cs[i][1]]
        elif op==8:  # move a chunk
            j=rng.randrange(len(cs)); c=cs.pop(j); cs.insert(rng.randrange(len(cs)+1),c)
        else:  # interlace flag flip with recompression of same data
            ih=bytearray(cs[0][1]); ih[12]^=1; cs[0][1]=bytes(ih)
        data=build(cs,crc,sig)
        path=f'{T}/m.png'; open(path,'wb').write(data)
        a,bb=run(old,path),run(new,path)
        key=('ok' if a[0]==0 else 'err')+('' if a==bb else '-DIFF'); stats[key]+=1
        if a!=bb: bad.append((f'{os.path.basename(f)}#{k}op{op}',a,bb)); open(f'{T}/diff{len(bad)}.png','wb').write(data)
print(dict(stats))
for l,a,b in bad[:30]: print('DIFF',l,'old',a,'| new',b)
