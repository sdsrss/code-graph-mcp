# Zero-answer measurement (D#229)

Does an empty caller answer list the calls it could not resolve, where the
zero is wrong? Measured against rust-analyzer's call pairs on tokio 1.41.1.

```bash
W=/var/tmp/d229                      # outside the repo; tokio copies, ~20 MB
# tokio at bb7ca75, twice: rust-analyzer writes target/ into what it indexes
evals/steering/tokio/template.sh $W/t && cp -a $W/t/tokio $W/tokio-scip
(cd $W/tokio-scip && rust-analyzer scip . --output $W/tokio.scip)
python3 scripts/scip_oracle/oracle.py --scip $W/tokio.scip --root $W/t/tokio \
    --dump-gold $W/gold.json
python3 scripts/zero_answer/population.py --db $W/t/tokio/.code-graph/index.db \
    --gold $W/gold.json --out $W/pop.json --names-out $W/names.txt
CG_D229_ROOT=$W/t/tokio CG_D229_NAMES=$W/names.txt CG_D229_OUT=$W/sites.jsonl \
    cargo test --release --test zero_answer_bench -- --ignored
python3 scripts/zero_answer/analyze.py $W/pop.json $W/sites.jsonl
```

Use a separate `CARGO_TARGET_DIR` for the release build if the plugin runs
this checkout's `target/release` binary. `ra_prod` is the truth to judge by:
the scan reads production code only, so a definition whose only callers are
tests cannot be found by it. Results and the decision they led to:
`tasks/specs/d229-zero-answer-disclosure.md`.
