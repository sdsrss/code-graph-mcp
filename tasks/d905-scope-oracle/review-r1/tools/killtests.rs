
#[test]
fn review_kill_m01_inner_comprehension_is_first_iterable() {
    // The inner comprehension is the outer one's first iterable: it runs
    // outside the outer scope, so `handler` there is the imported one.
    let refs = python_reference_names(
        "from m import handler\n\ndef f(z):\n    return [handler for handler in [use(handler) for _ in z]]\n",
    );
    assert!(refs.iter().any(|r| r == "handler"), "{refs:?}");
}

#[test]
fn review_kill_m06_builtin_named_renamed_import_call() {
    // `from m import a as set`: `set()` runs `m.a`, not the built-in.
    let calls = python_call_names("from m import a as set\n\ndef f():\n    set()\n");
    assert!(!calls.is_empty(), "{calls:?}");
}

#[test]
fn review_kill_m08_same_name_alias_of_a_builtin() {
    // `TimeoutError = TimeoutError` is the documented alias shape.
    let refs = python_reference_names("TimeoutError = TimeoutError\n\ndef f():\n    use(TimeoutError)\n");
    assert!(refs.iter().any(|r| r == "TimeoutError"), "{refs:?}");
}

#[test]
fn review_kill_m11_lambda_default_runs_outside_the_lambda() {
    // The default `helper()` runs in the module, not on the lambda's parameter.
    let rels = extract_relations("def helper():\n    pass\n\nf = lambda helper=helper(): helper\n", "python").unwrap();
    let call = rels.iter().find(|r| r.relation == REL_CALLS && r.target_name == "helper").expect("call kept");
    assert!(call.metadata.is_none(), "{:?}", call.metadata);
}

#[test]
fn review_kill_m12_def_default_runs_outside_the_def() {
    // `set()` in a default runs the built-in: the parameter `set` is not bound yet.
    let calls = python_call_names("def f(set=set()):\n    pass\n");
    assert!(calls.is_empty(), "{calls:?}");
}
