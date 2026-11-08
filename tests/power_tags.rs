//! Every tag of a case, written and read back through the public `Power` interface: a writable tag returns what was
//! written (booleans as 0/1), a read-only tag ignores writes and keeps its value.
use ferrofire::power::case::{load_file, RATE_A, RATE_C};
use ferrofire::power::{Islands, Power};
use std::path::PathBuf;

fn case30() -> ferrofire::power::case::Case {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/golden/case30.py");
    load_file(&p).unwrap_or_else(|e| panic!("{e} (run `make golden` first)"))
}

#[test]
fn writable_tags_read_back_and_read_only_tags_do_not_move() {
    let mut p = Power::new(case30(), Islands::Fail, false, &[1], &[1]).unwrap();
    assert!(
        p.tag_index("load-1_bus-1.mw").is_some() && p.tag_index("shunt-1_bus-1.active").is_some()
    );
    let mut checked = (0, 0);
    for i in 0..p.n_tags() {
        let before = p.get(i);
        for x in [0.0, 1.0] {
            p.set(i, x);
            if p.writable(i) {
                assert_eq!(p.get(i), x, "{} after writing {x}", p.tag_name(i));
                checked.0 += 1;
            } else {
                assert_eq!(p.get(i), before, "{} is read-only", p.tag_name(i));
                checked.1 += 1;
            }
        }
    }
    assert!(checked.0 > 400 && checked.1 > 100, "{checked:?}");
}

#[test]
fn nonzero_is_true_for_binary_fields_and_a_shunt_switched_off_reads_zero() {
    let mut p = Power::new(case30(), Islands::Fail, false, &[], &[]).unwrap();
    let on = p.tag_index("shunt-1_bus-5.active").unwrap();
    let mvar = p.tag_index("shunt-1_bus-5.actual_mvar").unwrap();
    let nominal = p.tag_index("shunt-1_bus-5.nominal_mvar").unwrap();
    let q = p.get(mvar);
    assert!(q != 0.0 && p.get(nominal) == q);
    p.set(on, 7.0);
    assert_eq!(p.get(on), 1.0);
    p.set(on, 0.0);
    assert_eq!((p.get(mvar), p.get(nominal)), (0.0, q));
    let g = p.tag_index("generator-1_bus-2.active").unwrap();
    p.set(g, -3.0);
    assert_eq!(p.get(g), 1.0); // status is stored as 0/1, any nonzero is "in service"
    p.set(g, 0.0);
    assert_eq!(p.get(g), 0.0);
}

#[test]
fn a_branch_rating_write_sets_all_three_rating_columns() {
    let mut p = Power::new(case30(), Islands::Fail, false, &[], &[]).unwrap();
    let i = p.tag_index("branch-1_1-2.mva_max").unwrap();
    p.set(i, 42.0);
    assert!((RATE_A..=RATE_C).all(|c| p.case.branch[0][c] == 42.0));
}

#[test]
fn drive_targets_on_missing_buses_are_refused() {
    let e = |l: &[i64], s: &[i64]| {
        Power::new(case30(), Islands::Fail, false, l, s)
            .err()
            .expect("should fail")
    };
    assert!(e(&[999], &[]).contains("load-1_bus-999: no bus 999"));
    assert!(e(&[], &[998]).contains("shunt-1_bus-998: no bus 998"));
}
