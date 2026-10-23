use crazyagent::worker_dispatch::FenceOutcome;
use std::mem::size_of;

#[test]
fn fence_outcome_inline_storage_fits_two_machine_words() {
    let inline_bytes = size_of::<FenceOutcome>();
    let budget_bytes = 2 * size_of::<usize>();
    assert!(
        inline_bytes <= budget_bytes,
        "FenceOutcome uses {inline_bytes} inline bytes; two-word budget is {budget_bytes} bytes"
    );
}
