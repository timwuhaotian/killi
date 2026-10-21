use shipping::{Quote, Zone, quote, zone_for};

#[test]
fn zones() {
    assert_eq!(zone_for("NZ"), Some(Zone::Oceania));
    assert_eq!(zone_for("DE"), Some(Zone::Europe));
    assert_eq!(zone_for("XX"), None);
}

#[test]
fn light_parcels() {
    // Europe, 400 g: tier 500 g = 570 cents; January fuel 348 bps -> 19 cents; 589 -> 590.
    assert_eq!(quote("FR", "75001", 400, 1), Some(Quote { zone: Zone::Europe, cents: 590 }));
    assert_eq!(quote("FR", "75001", 31_000, 1), None);
}

#[test]
fn heavy_parcels_pay_per_started_five_kilos() {
    // Europe, 10.5 kg: tier 15 kg = 450 + 120*8 = 1410; heavy fee 1 block = 350; total 1760.
    // March fuel for Europe is 333 bps -> 58 cents; 1818 -> 1820.
    assert_eq!(quote("DE", "10115", 10_500, 3), Some(Quote { zone: Zone::Europe, cents: 1820 }));
    // 20 kg exactly: 2 blocks = 700. Tier 20 kg = 450 + 120*9 = 1530; total 2230; fuel 74 -> 2304 -> 2305.
    assert_eq!(quote("DE", "10115", 20_000, 3), Some(Quote { zone: Zone::Europe, cents: 2305 }));
}
