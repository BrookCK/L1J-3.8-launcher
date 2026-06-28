use super::{lhx_tab_layout, LHX_BG};

#[test]
fn lhx_tab_layout_changes_size_by_selected_tab() {
    let potion = lhx_tab_layout(0);
    let delete = lhx_tab_layout(3);
    let misc = lhx_tab_layout(5);

    assert_ne!(potion.window_size, delete.window_size);
    assert_ne!(delete.window_size, misc.window_size);
    assert!(potion.window_size.0 >= potion.tabs_size.0);
    assert!(potion.window_size.1 >= potion.tabs_size.1);
}

#[test]
fn lhx_controls_use_white_background() {
    assert_eq!(LHX_BG, [255, 255, 255]);
}

#[test]
fn status_and_timer_keep_enough_height_for_bottom_controls() {
    assert!(lhx_tab_layout(2).window_size.1 >= 430);
    assert!(lhx_tab_layout(6).window_size.1 >= 430);
}

#[test]
fn lhx_visual_scale_has_4k_floor_even_when_dpi_is_unhelpful() {
    let scale = super::lhx_visual_scale(96, 3840, 2160);
    assert!(scale >= 1.35);
    assert!(super::lhx_font_size_for_scale(scale) >= 20);
}

#[test]
fn lhx_visual_scale_caps_windows_dpi_scaling_for_layout() {
    let scale = super::lhx_visual_scale(192, 2560, 1440);
    assert_eq!(scale, 1.25);
    assert_eq!(super::scale_px(485, scale), 606);
}

#[test]
fn owned_lhx_window_clears_global_topmost_style() {
    assert!(super::should_clear_lhx_topmost_after_owner_attached(true));
}

#[test]
fn standalone_lhx_window_keeps_existing_z_order() {
    assert!(!super::should_clear_lhx_topmost_after_owner_attached(false));
}

#[test]
fn lhx_owner_attach_is_disabled_for_game_window_stability() {
    assert!(!super::should_attach_lhx_to_game_owner());
}

#[test]
fn lhx_temporarily_hides_while_owned_game_window_is_minimized() {
    assert_eq!(
        super::desired_lhx_visibility(super::VISIBLE_SHOWN, true),
        Some(false)
    );
    assert_eq!(
        super::desired_lhx_visibility(super::VISIBLE_SHOWN, false),
        Some(true)
    );
}

#[test]
fn long_combo_dropdowns_use_multiple_visible_rows() {
    assert_eq!(super::combo_dropdown_visible_rows(0), 1);
    assert_eq!(super::combo_dropdown_visible_rows(4), 4);
    assert_eq!(super::combo_dropdown_visible_rows(40), 40);
    assert_eq!(super::combo_dropdown_visible_rows(70), 50);
}

#[test]
fn delete_combo_dropdown_keeps_visible_rows_capped_for_scrolling() {
    assert_eq!(super::delete_combo_dropdown_visible_rows(0), 1);
    assert_eq!(super::delete_combo_dropdown_visible_rows(15), 15);
    assert_eq!(super::delete_combo_dropdown_visible_rows(50), 50);
    assert_eq!(super::delete_combo_dropdown_visible_rows(70), 50);
}

#[test]
fn delete_list_entry_name_strips_stack_quantity_before_saving() {
    assert_eq!(super::delete_list_entry_name("肉 (191)"), "肉");
    assert_eq!(super::delete_list_entry_name("金幣 (17,099)"), "金幣");
}

#[test]
fn delete_list_entry_name_keeps_non_count_parentheses() {
    assert_eq!(
        super::delete_list_entry_name("精靈水晶(水之元氣)"),
        "精靈水晶(水之元氣)"
    );
}
