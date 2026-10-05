import os,sys,statistics,random
# usage: pair.py N binA binB 'args with {in} {out}' inputs...
n=int(sys.argv[1]); A,B=sys.argv[2],sys.argv[3]; tmpl=sys.argv[4].split(); inputs=sys.argv[5:]
T='/tmp/b'; out=f'{T}/p.flac'
def run(b,inp):
    if os.path.exists(out): os.unlink(out)
    c=[b]+[a.replace('{in}',inp).replace('{out}',out) for a in tmpl]
    pid=os.posix_spawn(b,c,os.environ,file_actions=[(os.POSIX_SPAWN_OPEN,1,'/dev/null',os.O_WRONLY,0)])
    _,st,ru=os.wait4(pid,0); assert st==0,c
    return ru.ru_utime+ru.ru_stime
for inp in inputs:
    run(A,inp); run(B,inp); d=[]; ta=[]; tb=[]
    for i in range(n):
        if i%2: b=run(B,inp); a=run(A,inp)
        else: a=run(A,inp); b=run(B,inp)
        d.append(b/a-1); ta.append(a); tb.append(b)
    random.seed(1); bs=sorted(statistics.median(random.choices(d,k=len(d))) for _ in range(2000))
    print(f"{' '.join(tmpl[:1]+[x for x in tmpl[1:] if x.startswith('--')]):<22} {os.path.basename(inp):<14} A={statistics.median(ta):.3f} B={statistics.median(tb):.3f} {statistics.median(d)*100:+.1f}% CI[{bs[50]*100:+.1f},{bs[1950]*100:+.1f}]",flush=True)
