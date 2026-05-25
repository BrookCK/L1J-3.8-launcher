#[cfg(test)]
use crate::aux::input_sim::{
    gameplay_click_point_from_offset, gameplay_click_point_from_player_anchor_offset,
};
use crate::bot::perception::position::PlayerPosition;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPoint {
    pub x: i32,
    pub y: i32,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileClickDiagnostic {
    pub dx_tile: i32,
    pub dy_tile: i32,
    pub dx_px: i32,
    pub dy_px: i32,
    pub legacy_center: ClientPoint,
    pub player_anchor: ClientPoint,
}

#[cfg(test)]
pub fn tile_click_diagnostic(
    client_width: i32,
    client_height: i32,
    player: PlayerPosition,
    target_x: i32,
    target_y: i32,
) -> Option<TileClickDiagnostic> {
    let (dx_px, dy_px) = tile_click_offset(player, target_x, target_y);
    Some(TileClickDiagnostic {
        dx_tile: target_x - player.x,
        dy_tile: target_y - player.y,
        dx_px,
        dy_px,
        legacy_center: legacy_center_point(client_width, client_height, dx_px, dy_px)?,
        player_anchor: player_anchor_point(client_width, client_height, dx_px, dy_px)?,
    })
}

pub fn tile_click_offset(player: PlayerPosition, target_x: i32, target_y: i32) -> (i32, i32) {
    tile_delta_to_click_offset(target_x - player.x, target_y - player.y)
}

pub fn walk_drag_offset(player: PlayerPosition, target_x: i32, target_y: i32) -> (i32, i32) {
    tile_click_offset(player, target_x, target_y)
}

fn tile_delta_to_click_offset(dx_tile: i32, dy_tile: i32) -> (i32, i32) {
    const ISO_X_PER_TILE: i32 = 32;
    const ISO_Y_PER_TILE: i32 = 16;
    (
        (dx_tile + dy_tile) * ISO_X_PER_TILE,
        (dy_tile - dx_tile) * ISO_Y_PER_TILE,
    )
}

#[cfg(test)]
fn legacy_center_point(
    client_width: i32,
    client_height: i32,
    dx_px: i32,
    dy_px: i32,
) -> Option<ClientPoint> {
    gameplay_click_point_from_offset(client_width, client_height, dx_px, dy_px)
        .map(|(x, y)| ClientPoint { x, y })
}

#[cfg(test)]
fn player_anchor_point(
    client_width: i32,
    client_height: i32,
    dx_px: i32,
    dy_px: i32,
) -> Option<ClientPoint> {
    gameplay_click_point_from_player_anchor_offset(client_width, client_height, dx_px, dy_px)
        .map(|(x, y)| ClientPoint { x, y })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_uses_player_anchor_not_client_center() {
        let player = PlayerPosition { x: 1000, y: 1000 };

        let diag = tile_click_diagnostic(800, 600, player, 1001, 1000).expect("diagnostic");

        assert_eq!(diag.dx_tile, 1);
        assert_eq!(diag.dy_tile, 0);
        assert_eq!(diag.dx_px, 32);
        assert_eq!(diag.dy_px, -16);
        assert_eq!(diag.legacy_center, ClientPoint { x: 432, y: 284 });
        assert_eq!(diag.player_anchor, ClientPoint { x: 432, y: 224 });
    }

    #[test]
    fn diagnostic_tracks_actual_client_size() {
        let player = PlayerPosition { x: 1000, y: 1000 };

        let diag = tile_click_diagnostic(1904, 1041, player, 999, 999).expect("diagnostic");

        assert_eq!(diag.dx_px, -64);
        assert_eq!(diag.dy_px, 0);
        assert_eq!(diag.legacy_center, ClientPoint { x: 888, y: 520 });
        assert_eq!(diag.player_anchor, ClientPoint { x: 888, y: 416 });
    }

    #[test]
    fn far_target_is_clamped_inside_gameplay_area() {
        let player = PlayerPosition { x: 1000, y: 1000 };

        let diag = tile_click_diagnostic(800, 600, player, 1100, 1100).expect("diagnostic");

        assert_eq!(diag.player_anchor, ClientPoint { x: 599, y: 240 });
    }

    #[test]
    fn lineage_iso_projection_maps_diagonal_world_to_horizontal_screen() {
        let player = PlayerPosition { x: 1000, y: 1000 };

        assert_eq!(tile_click_offset(player, 1001, 1001), (64, 0));
        assert_eq!(tile_click_offset(player, 999, 999), (-64, 0));
        assert_eq!(tile_click_offset(player, 1001, 999), (0, -32));
        assert_eq!(tile_click_offset(player, 999, 1001), (0, 32));
    }

    #[test]
    fn walk_drag_offset_keeps_adjacent_waypoint_exact_for_path_following() {
        let player = PlayerPosition { x: 1000, y: 1000 };

        assert_eq!(walk_drag_offset(player, 1001, 1000), (32, -16));
        assert_eq!(walk_drag_offset(player, 1000, 1001), (32, 16));
        assert_eq!(walk_drag_offset(player, 999, 1000), (-32, 16));
        assert_eq!(walk_drag_offset(player, 1000, 999), (-32, -16));
    }
}
