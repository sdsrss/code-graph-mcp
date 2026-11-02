for d in k_*; do r=$(cd $d && timeout 10 python3 -I -B -c "
import sys; sys.path.insert(0,'.')
try:
    import a
    print(a.t_call())
except Exception as e: print('ERR', type(e).__name__)
" 2>&1 | tail -1); echo "$d $r"; done
