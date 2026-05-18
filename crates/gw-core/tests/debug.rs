use gw_core::debug::TraceBuffer;

#[test]
fn phase_start_end_records_alloc_and_time() {
    let mut tb = TraceBuffer::new();
    tb.phase_start("p1", 100);
    std::thread::sleep(std::time::Duration::from_micros(50));
    tb.phase_end(200);
    assert_eq!(tb.phase_count, 1);
    let p = &tb.phases[0];
    assert_eq!(p.name, "p1");
    assert!(p.end_ns > p.start_ns);
    assert_eq!(p.alloc_at_start, 100);
    assert_eq!(p.alloc_at_end, 200);
}

#[test]
fn phase_overflow_truncates_silently() {
    let mut tb = TraceBuffer::new();
    for i in 0..40 {
        tb.phase_start("p", i);
        tb.phase_end(i);
    }
    assert_eq!(tb.phase_count, 32);
}
