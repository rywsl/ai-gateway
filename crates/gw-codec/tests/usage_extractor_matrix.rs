use gw_codec::usage_extractor::{ExtractedUsage, ProvenanceTag, UsageExtractor};

#[test]
fn extractor_trait_object_safe() {
    fn _accept(_: &dyn UsageExtractor) {}
    let _ = _accept;
}

#[test]
fn provenance_default_is_unknown() {
    let tag = ProvenanceTag::default();
    assert!(matches!(tag, ProvenanceTag::Unknown));
}

#[test]
fn extracted_usage_default_is_zero_unknown_nonterminal() {
    let e = ExtractedUsage::default();
    assert_eq!(e.values.input_tokens, 0);
    assert_eq!(e.values.output_tokens, 0);
    assert!(matches!(e.provenance.input, ProvenanceTag::Unknown));
    assert!(!e.is_terminal);
}
