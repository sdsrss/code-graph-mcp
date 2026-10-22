import statistics as st, collections, sys
f=sys.argv[1]; A=sys.argv[2]; B=sys.argv[3]
rows=[l.rstrip('\n').split('\t') for l in open(f)]
d=collections.defaultdict(lambda: collections.defaultdict(list))
for n,arm,i,ms,wall,u,s,load in rows:
    d[n][arm].append((int(ms),float(wall),float(u)+float(s),float(load)))
for n in d:
    out=[]
    for arm in [A,B]:
        v=d[n][arm]; ms=[x[0] for x in v]; cpu=[x[2] for x in v]
        out.append((arm, st.median(ms), min(ms), max(ms), st.median(cpu), min(cpu), max(cpu), len(v)))
    b,c=out
    print(f"{n:9s} elapsed_ms {A} med {b[1]:.0f} [{b[2]}-{b[3]}] {B} med {c[1]:.0f} [{c[2]}-{c[3]}] ({(c[1]-b[1])/b[1]*100:+.1f}%) | cpu_s {A} {b[4]:.2f} [{b[5]:.2f}-{b[6]:.2f}] {B} {c[4]:.2f} [{c[5]:.2f}-{c[6]:.2f}] ({(c[4]-b[4])/b[4]*100:+.1f}%) n={b[7]}/{c[7]}")
print('load range', min(float(r[7]) for r in rows), max(float(r[7]) for r in rows))
