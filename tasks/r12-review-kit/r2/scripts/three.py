import sqlite3, sys
sys.argv=[sys.argv[0]]+sys.argv[1:]
exec(open('/var/tmp/agent-tmp-r12-review2/scripts/pairs.py').read().split("args = [")[0])
v, r, h = (load(f'snap/{sys.argv[1]}.{a}.db') for a in ('v0193','rel','r1'))
add = [k for k in h if k not in r and k[0]=='calls']
rem = [k for k in r if k not in h and k[0]=='calls']
print('added rel->r1:', len(add), 'present in v0193:', sum(k in v for k in add), 'same conf as v0193:', sum(k in v and v[k]==h[k] for k in add))
for k in add:
    if k not in v or v[k]!=h[k]: print('  differs from v0193:', k, v.get(k), h[k])
print('removed rel->r1:', len(rem), 'present in v0193:', sum(k in v for k in rem))
for k in rem: print('  ', k, 'v0193:', v.get(k), 'rel:', r[k])
