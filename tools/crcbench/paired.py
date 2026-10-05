import os,sys,statistics,resource,subprocess,random
# usage: paired.py pairs binA binB inputs...  (frame-copy convert); prints median paired CPU diff and 95% bootstrap CI
n=int(sys.argv[1]); A,B=sys.argv[2],sys.argv[3]; inputs=sys.argv[4:]
tmp=os.environ['BENCH_TMP']
def run(b,inp):
    out=os.path.join(tmp,'p.flac')
    if os.path.exists(out): os.unlink(out)
    pid=os.posix_spawn(b,[b,'convert',inp,out],os.environ,file_actions=[(os.POSIX_SPAWN_OPEN,1,'/dev/null',os.O_WRONLY,0)])
    _,st,ru=os.wait4(pid,0); assert st==0
    return ru.ru_utime+ru.ru_stime
for inp in inputs:
    run(A,inp); run(B,inp)
    d=[]; ta=[]; tb=[]
    for i in range(n):
        if i%2: b=run(B,inp); a=run(A,inp)
        else: a=run(A,inp); b=run(B,inp)
        d.append(b/a-1); ta.append(a); tb.append(b)
    random.seed(1)
    boots=sorted(statistics.median(random.choices(d,k=len(d))) for _ in range(2000))
    print(f"{os.path.basename(inp):<14} A={statistics.median(ta):.4f} B={statistics.median(tb):.4f} paired median {statistics.median(d)*100:+.2f}%  95%CI [{boots[50]*100:+.2f}%, {boots[1950]*100:+.2f}%]",flush=True)
