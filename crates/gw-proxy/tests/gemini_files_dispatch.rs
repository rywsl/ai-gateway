#[test]
fn gemini_files_routes_strategy_kinds_through_strategy_choice() {
    let source = include_str!("../src/filters/gemini_files.rs");

    assert_eq!(
        source
            .matches(".strategy_for_kind(&group.value.strategy)")
            .count(),
        2,
        "both Gemini Files runtime dispatch sites must route through StrategyChoice"
    );
    assert!(
        !source.contains(".pick_for(&group.value.strategy)"),
        "Gemini Files must not bypass inline multi_dim by calling pick_for directly"
    );
}
