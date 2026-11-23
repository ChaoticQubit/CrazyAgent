use super::*;

/// Draws a view once, then removes the D3D12 device right before its next present and
/// waits until the seam has removed it, so `Present` itself is the first call to find the
/// loss. Inputs: the parent and the webview. Output: the view, id 1.
fn removed_inside_present(parent: HWND, webview: HWND) -> View {
    let view = View::new(parent, webview, 1, sink, &style()).expect("a view");
    let before = presents();
    assert!(view.feed(fed_frame(&Theme::default(), b"before the removal\r\n")));
    view.place(Some(PAGE)).expect("a box");
    await_present("the present before the removal", before);
    assert_eq!(
        draw::trial_removals(),
        0,
        "no removal before the seam is armed"
    );
    draw::trial_remove_at_present(1);
    assert!(view.feed(fed_frame(&Theme::default(), b"after the removal\r\n")));
    pump_until("the seam removed the device", FARM_WAIT, || {
        draw::trial_removals() == 1
    });
    view
}

#[test]
#[ignore = "needs a Windows desktop and a process of its own, since a removed D3D12 device \
            stays removed for the process: the farm runs it alone"]
fn a_device_removed_inside_present_faults_the_view() {
    let (parent, webview) = stand_ins();
    let view = removed_inside_present(parent, webview);
    pump_until("the removal faults the view", FARM_WAIT, || view.faulted());
    let (presents, failures) = draw::counts();
    println!("device-lost row: view 1 faulted, presents {presents}, failures {failures}");
    dropped_within_the_bound(view, "the faulted view");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes last");
}

#[test]
#[ignore = "needs a Windows desktop and a process of its own, since a removed D3D12 device \
            stays removed for the process: the farm runs it alone"]
fn the_render_thread_answers_a_view_placed_after_a_removal_inside_present() {
    let (parent, webview) = stand_ins();
    let removed = removed_inside_present(parent, webview);
    let before = presents();
    let later = View::new(parent, webview, 2, sink, &style()).expect("a later view");
    assert!(later.feed(fed_frame(&Theme::default(), b"a later view\r\n")));
    later
        .place(Some((420.0, 40.0, 200.0, 100.0)))
        .expect("a second box");
    pump_until(
        "the render thread answers the later view",
        FARM_WAIT,
        || later.faulted() || presents() > before,
    );
    assert!(
        !later.faulted() && presents() > before,
        "a replacement device comes up after a removal: the later view presents and has not faulted"
    );
    released_within_the_bound();
    println!(
        "device-lost row: a replacement device after the removal came up, presents {}",
        presents()
    );
    dropped_within_the_bound(later, "the later view");
    dropped_within_the_bound(removed, "the removed view");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes last");
}
