import random, sys, importlib.util, json
gen_path, seed, step = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
spec = importlib.util.spec_from_file_location("gen", gen_path); gen = importlib.util.module_from_spec(spec); spec.loader.exec_module(gen)
rng = random.Random(seed); st = gen.initial(rng)
for i in range(step): st = gen.step(st, rng)
out = sys.argv[4]
open(out, "w").write("CASES = " + repr({f"seed{seed}-step{step}": st}) + "\n")
