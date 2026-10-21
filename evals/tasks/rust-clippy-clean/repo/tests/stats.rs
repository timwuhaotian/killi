use stats::{Summary, label, parse_numbers, summarize};

#[test]
fn summarizes() {
    let values = parse_numbers("3, 1 4,1  5 x 9");
    assert_eq!(values, vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0]);
    let summary = summarize(&values).unwrap();
    assert_eq!(summary, Summary { count: 6, mean: 23.0 / 6.0, min: 1.0, max: 9.0 });
    assert_eq!(summarize(&Vec::new()), None);
}

#[test]
fn labels() {
    let summary = summarize(&parse_numbers("1 2 9")).unwrap();
    assert_eq!(label(9.0, &summary), "max");
    assert_eq!(label(5.0, &summary), "above");
    assert_eq!(label(1.0, &summary), "min");
    assert_eq!(label(2.0, &summary), "below");
}
