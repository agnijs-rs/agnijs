//! Every target runs its seeds and a few mutants, so the targets and their
//! seeds stay working between fuzz runs. The fuzzing itself is the
//! `agnijs-fuzz` binary (docs/fuzzing.md).

use agnijs_fuzz::{mutate, Shape, TARGETS};

#[test]
fn every_target_runs_its_seeds_and_some_mutants() {
    let mut rng = mutate::Rng::new(7);
    for t in TARGETS {
        let seeds = (t.seeds)();
        assert!(!seeds.is_empty(), "{} has no seeds", t.name);
        for s in &seeds {
            (t.run)(s);
        }
        for _ in 0..10 {
            let base = rng.pick(&seeds);
            let input = match t.shape {
                Shape::Text => mutate::text(&mut rng, base, &seeds),
                Shape::Binary => mutate::binary(&mut rng, base, &seeds),
            };
            (t.run)(&input);
        }
    }
}

/// The binary targets rebuild a container around the input; their seeds
/// must get past the loader, or mutation only ever tests the refusal.
#[test]
fn container_seeds_load() {
    use agnijs_fuzz::seeds;
    for tree in seeds::bytecode_trees() {
        let container = agnijs_fuzz::bytecode_container(&tree);
        let mut e = agnijs_js_vm::make_engine(agnijs_fuzz::FuzzHost::new());
        let realm = e.main_realm();
        let loaded = e.eval_bytecode(realm, &container);
        assert!(
            !matches!(loaded, Err(agnijs_engine_api::Error::BytecodeCorrupt(_) | agnijs_engine_api::Error::BytecodeMismatch { .. })),
            "a bytecode seed is refused: {loaded:?}"
        );
    }
    let body = agnijs_fuzz::snapshot_body();
    let restored = agnijs_js_vm::make_engine_from_snapshot(agnijs_fuzz::FuzzHost::new(), &agnijs_fuzz::snapshot_container(&body));
    assert!(restored.is_ok(), "the snapshot seed is refused");
    for p in seeds::regex_programs() {
        assert!(agnijs_js_regex::Regex::from_program_bytes(&p, agnijs_js_regex::MatcherMode::Auto).is_ok());
    }
    assert!(seeds::regex_programs().len() >= seeds::REGEX.len() - 1);
}

#[test]
fn target_names_are_unique() {
    for (i, t) in TARGETS.iter().enumerate() {
        assert!(TARGETS[..i].iter().all(|u| u.name != t.name), "{} is listed twice", t.name);
        assert!(agnijs_fuzz::target(t.name).is_some());
    }
}
