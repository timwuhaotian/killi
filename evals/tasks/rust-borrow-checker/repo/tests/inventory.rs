use inventory::Inventory;

#[test]
fn add_merges_and_take_subtracts() {
    let mut inv = Inventory::new();
    inv.add("apple", 3);
    inv.add("pear", 1);
    inv.add("apple", 2);
    assert_eq!(inv.quantity("apple"), 5);
    assert_eq!(inv.take("apple", 4), Ok(1));
    assert_eq!(inv.take("apple", 2), Err("only 1 apple left".to_string()));
    assert_eq!(inv.take("plum", 1), Err("no plum in stock".to_string()));
}

#[test]
fn remove_empty_drops_every_zero_entry() {
    let mut inv = Inventory::new();
    for name in ["a", "b", "c", "d"] {
        inv.add(name, 1);
    }
    inv.take("a", 1).unwrap();
    inv.take("b", 1).unwrap();
    inv.take("d", 1).unwrap();
    inv.remove_empty();
    assert_eq!(inv.quantity("c"), 1);
    assert_eq!(inv.most_stocked(), Some("c"));
    inv.take("c", 1).unwrap();
    inv.remove_empty();
    assert_eq!(inv.most_stocked(), None);
}

#[test]
fn most_stocked_picks_the_largest() {
    let mut inv = Inventory::new();
    inv.add("x", 2);
    inv.add("y", 9);
    inv.add("z", 4);
    assert_eq!(inv.most_stocked(), Some("y"));
}
