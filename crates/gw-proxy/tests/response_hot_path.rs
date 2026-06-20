//! Static-source assertions for the response hot-path: the memory-mode
//! dispatch must come before the `block_in_place` disk-spill bridge, and
//! `run_memory_sync` itself must be fully synchronous (no `.await`, no
//! `block_on`) so it never defeats the sync hot-path.

fn function_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src.find(name).expect("function exists");
    let after = &src[start..];
    let mut depth = 0usize;
    let mut seen_open = false;
    for (idx, ch) in after.char_indices() {
        match ch {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' if seen_open => {
                depth -= 1;
                if depth == 0 {
                    return &after[..=idx];
                }
            }
            _ => {}
        }
    }
    panic!("function body not found");
}

#[test]
fn response_body_filter_selects_memory_before_block_in_place() {
    let src = include_str!("../src/gateway.rs");
    let body = function_body(src, "fn response_body_filter");
    let memory_branch = body
        .find("run_memory_sync")
        .expect("memory sync branch present");
    let bridge = body
        .find("tokio::task::block_in_place")
        .expect("disk-spill bridge present");
    assert!(
        memory_branch < bridge,
        "memory-mode dispatch must come before the block_in_place bridge"
    );
}

#[test]
fn response_memory_filter_is_sync() {
    let src = include_str!("../src/filters/response.rs");
    let body = function_body(src, "pub fn run_memory_sync");
    assert!(
        !body.contains(".await"),
        "run_memory_sync must not contain `.await` (would defeat sync hot-path)"
    );
    assert!(
        !body.contains("block_on"),
        "run_memory_sync must not call `block_on`"
    );
}
