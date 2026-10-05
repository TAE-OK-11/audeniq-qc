import os,sys,random,subprocess,glob,collections
# usage: imgdiff.py OLD NEW N_MUT files...
old,new,nmut=sys.argv[1],sys.argv[2],int(sys.argv[3]); files=sys.argv[4:]
T=os.environ.get('T','/tmp/imgdiff'); os.makedirs(T,exist_ok=True)
def run(b,p):
    r=subprocess.run([b,'image-probe',p],capture_output=True,timeout=60)
    return (r.returncode, r.stdout if r.returncode>=0 else b'SIGNAL')
stats=collections.Counter(); bad=[]
def check(p,label):
    a,b=run(old,p),run(new,p)
    key=('ok' if a[0]==0 else 'err')+('' if a==b else '-DIFF')
    stats[key]+=1
    if a!=b: bad.append((label,a,b))
rng=random.Random(1234)
for f in files:
    check(f,os.path.basename(f))
    data=open(f,'rb').read(); ext=os.path.splitext(f)[1]
    for k in range(nmut):
        d=bytearray(data); op=rng.randrange(6)
        if op==0:
            for _ in range(rng.randint(1,4)):
                i=rng.randrange(len(d)); d[i]^=1<<rng.randrange(8)
        elif op==1: d=d[:rng.randrange(len(d))]
        elif op==2:
            i=rng.randrange(len(d)); d[i]=rng.choice([0,255,0xd9,0xd0,0xda,0xc4,0xdb,rng.randrange(256)])
        elif op==3:
            i=rng.randrange(len(d)); d[i:i]=bytes(rng.randrange(256) for _ in range(rng.randint(1,8)))
        elif op==4:
            i=rng.randrange(len(d)); del d[i:i+rng.randint(1,16)]
        else:
            i=rng.randrange(max(1,len(d)-1)); j=rng.randrange(len(d)); d[i]=d[j]
        p=f'{T}/m{ext}'; open(p,'wb').write(d)
        check(p,f'{os.path.basename(f)}#{k}op{op}')
        if (key:=bad and bad[-1][0]) and key.startswith(os.path.basename(f)+'#'+str(k)+'o'):
            open(f'{T}/diff{len(bad)}{ext}','wb').write(d)
print(dict(stats)); print("non-panic diffs:", sum(1 for _,a,b in bad if a[0]>=0))
for label,a,b in [x for x in bad if x[1][0]>=0][:25]: print('DIFF',label,'old',a[0],a[1][:90],'| new',b[0],b[1][:90])
