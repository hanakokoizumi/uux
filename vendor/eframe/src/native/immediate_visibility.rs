//! A visible immediate child needs its parent's UI even without a parent redraw.
pub(crate) fn run_parent_ui(full_pass: bool, parent_visible: bool, child_visible: bool) -> bool {
    child_visible || (full_pass && parent_visible)
}

#[cfg(test)]
mod tests {
    use super::run_parent_ui;

    #[test]
    fn visible_child_runs_even_without_parent_frame_callback() {
        assert!(run_parent_ui(false, false, true));
        assert!(run_parent_ui(true, false, true));
        assert!(!run_parent_ui(false, true, false));
        assert!(!run_parent_ui(true, false, false));
        assert!(run_parent_ui(true, true, false));
    }
}
