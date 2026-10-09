//! The committed `idls/ad_pool.json` must equal the IDL of the program being built.
//! Fix: `anchor build && scripts/export-idl.sh`.

#[test]
fn exported_idl_matches_built_idl() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let built =
        std::fs::read(format!("{root}/target/idl/ad_pool.json")).expect("run `anchor build` first");
    let exported = std::fs::read(format!("{root}/idls/ad_pool.json"))
        .expect("idls/ad_pool.json missing — run scripts/export-idl.sh");
    assert!(
        built == exported,
        "idls/ad_pool.json is stale — run `anchor build && scripts/export-idl.sh`"
    );
}

#[test]
fn idl_lists_every_instruction_and_account() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let idl: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{root}/idls/ad_pool.json")).unwrap(),
    )
    .unwrap();
    let names = |key: &str| -> Vec<String> {
        let mut v: Vec<String> = idl[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names("instructions"),
        [
            "add_slot",
            "approve_creative",
            "approve_image",
            "bid",
            "close_auction",
            "init_ad_config",
            "open_auction",
            "reject_creative",
            "revoke_creative",
            "set_slot_active",
            "settle",
            "update_ad_config",
        ]
    );
    assert_eq!(names("accounts"), ["AdSlotAuction", "GameAdConfig"]);
    assert_eq!(names("events").len(), 11);
    assert_eq!(idl["errors"].as_array().unwrap().len(), 32);
}
