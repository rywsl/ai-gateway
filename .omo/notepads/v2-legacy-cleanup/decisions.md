# Decisions — v2-legacy-cleanup

## 2026-05-25 Plan Start
- P1: -> ! not viable on stable Rust 1.86 → bail! macro wrapping emit_and_bail + unreachable!()
- P4: All 6 #[allow(dead_code)] are justified KEEP — NO ACTION
- P5: REMOVE 4 placeholder tests (Python E2E covers), KEEP e2e_tunnel.rs
- W4: Use exhaustive ApiShape match, NO wildcard _ =>
- W6: Remove _Phantom entirely, NOT implement Phase 2 wire types
- Commit strategy: doc sync via git commit --fixup + autosquash
