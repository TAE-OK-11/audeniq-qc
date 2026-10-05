import struct,random,sys,os
out=sys.argv[1]; rng=random.Random(5)
def seg(m,b): return bytes([0xff,m])+struct.pack('>H',len(b)+2)+b
class W:
    def __init__(s): s.o=bytearray(); s.acc=0; s.n=0
    def put(s,v,n):
        for i in range(n-1,-1,-1):
            s.acc=(s.acc<<1)|((v>>i)&1); s.n+=1
            if s.n==8:
                s.o.append(s.acc)
                if s.acc==0xff: s.o.append(0)
                s.acc=0; s.n=0
    def flush(s):
        if s.n: s.put((1<<(8-s.n))-1,8-s.n)
# DC table: 17 symbols, all length 5 -> codes 0..16
counts=[0]*16; counts[4]=17
dht=bytes([0x00])+bytes(counts)+bytes(range(17))
def make(P,nf,pred,scans,ri,w,h,maxcat=16):
    sof=bytes([P])+struct.pack('>HH',h,w)+bytes([nf])+b''.join(bytes([c+1,0x11,0]) for c in range(nf))
    o=bytearray(b'\xff\xd8')+seg(0xc4,dht)+seg(0xc3,sof)
    if ri: o+=seg(0xdd,struct.pack('>H',ri))
    for comps in scans:
        sos=bytes([len(comps)])+b''.join(bytes([c+1,0x00]) for c in comps)+bytes([pred,0,0])
        o+=seg(0xda,sos)
        wr=W(); k=0; rst=0
        for y in range(h):
            for x in range(w):
                if ri and k and k%ri==0:
                    wr.flush(); o+=wr.o; wr=W(); o+=bytes([0xff,0xd0+rst]); rst=(rst+1)%8
                k+=1
                for c in comps:
                    s=rng.randrange(0,min(maxcat,P)+1) if rng.random()<.7 else 0
                    wr.put(s,5)
                    if 1<=s<=15: wr.put(rng.randrange(1<<s),s)
        wr.flush(); o+=wr.o
    o+=b'\xff\xd9'
    return bytes(o)
i=0
for P in (2,4,7,8,9,12,16):
    for nf in (1,3,4):
        for split in (False,True):
            scans=[[c] for c in range(nf)] if split else [list(range(nf))]
            for pred in (0,1,6):
                for ri in (0,5):
                    open(f'{out}/ll_P{P}_n{nf}_{"s" if split else "i"}_p{pred}_r{ri}.jpg','wb').write(make(P,nf,pred,scans,ri,13,7)); i+=1
# a 3-component frame with only some components in scans
open(f'{out}/ll_partial.jpg','wb').write(make(8,3,1,[[0],[1]],0,9,5))
open(f'{out}/ll_partial2.jpg','wb').write(make(8,3,1,[[0,1]],0,9,5))
print(i+2)
