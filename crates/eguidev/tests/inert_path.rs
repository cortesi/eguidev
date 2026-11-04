//! The inert path of the instrumentation allocates no more than plain egui.

#[cfg(test)]
mod tests {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    use eguidev::{DevMcp, DevUiExt, FrameGuard};

    /// Forwards to the system allocator, and counts the allocations of a thread
    /// that counts.
    struct CountingAllocator;

    thread_local! {
        /// The allocations of this thread since `count` started, while it runs.
        static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// Count one allocation of this thread.
    fn note_allocation() {
        let _counted = ALLOCATIONS.try_with(|allocations| {
            if let Some(count) = allocations.get() {
                allocations.set(Some(count + 1));
            }
        });
    }

    // SAFETY: each method forwards its arguments to the system allocator
    // unchanged.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note_allocation();
            // SAFETY: the caller upholds the contract of `GlobalAlloc::alloc`.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            note_allocation();
            // SAFETY: the caller upholds the contract of
            // `GlobalAlloc::alloc_zeroed`.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note_allocation();
            // SAFETY: the caller upholds the contract of
            // `GlobalAlloc::realloc`.
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: the caller upholds the contract of
            // `GlobalAlloc::dealloc`.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    /// Return the count of the allocations that `f` makes on this thread.
    fn count(f: impl FnOnce()) -> usize {
        ALLOCATIONS.with(|allocations| allocations.set(Some(0)));
        f();
        ALLOCATIONS
            .with(|allocations| allocations.replace(None))
            .unwrap_or(0)
    }

    /// Widgets of each block.
    const WIDGETS: usize = 64;

    /// Show a block of widgets, through the instrumentation or as plain egui.
    fn block(ui: &mut egui::Ui, instrumented: bool) {
        let mut checked = false;
        let mut value = 0.5_f32;
        for _ in 0..WIDGETS {
            if instrumented {
                let _button = ui.dev_button("inert.button", "Inert button");
                let _label = ui.dev_label("inert.label", "Inert label");
                let _checkbox = ui.dev_checkbox("inert.checkbox", &mut checked, "Inert box");
                let _slider = ui.dev_slider("inert.slider", &mut value, 0.0..=1.0);
            } else {
                let _button = ui.button("Inert button");
                let _label = ui.label("Inert label");
                let _checkbox = ui.checkbox(&mut checked, "Inert box");
                let _slider = ui.add(egui::Slider::new(&mut value, 0.0..=1.0));
            }
        }
    }

    #[test]
    fn the_inert_path_allocates_no_more_than_plain_egui() {
        let devmcp = DevMcp::new();
        let ctx = egui::Context::default();
        let mut counts = (0, 0);
        // The first frames warm the caches of egui, so that the measured frame
        // lays out each text from its cache.
        for frame in 0..3 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let ctx = ui.ctx().clone();
                let _guard = FrameGuard::new(&devmcp, &ctx);
                let plain = count(|| block(ui, false));
                let instrumented = count(|| block(ui, true));
                if frame == 2 {
                    counts = (plain, instrumented);
                }
            })
            .drop_without_applying_deltas();
        }
        let (plain, instrumented) = counts;
        // Each instrumented widget used to allocate its ID and its label, which
        // is two allocations for each of the WIDGETS * 4 widgets of a
        // block. A few allocations of slack absorb the growth of the
        // paint list of egui.
        assert!(
            instrumented <= plain + 16,
            "instrumented block allocated {instrumented} times, plain egui {plain} times"
        );
    }
}
