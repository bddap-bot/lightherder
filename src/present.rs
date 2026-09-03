//! Copying the monitors to whatever is watching them — a window, or a
//! texture a test can read back. Several monitors share the target as a
//! grid of tiles, each letterboxed in its cell.

use crate::feedback::Feedback;

/// What of the bank the display shows: the whole of it tiled, with the
/// monitor the front panel plays picked out, or one monitor alone. One value
/// rather than a solo beside a focus, so a solo of one monitor with the mark
/// on another cannot be asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Every monitor, tiled. `focus` is the one the front panel plays, and
    /// its tile is framed with a line so a glance finds which glass the
    /// faders are on; `None` frames nothing, for a target that is not a
    /// display.
    Bank { focus: Option<usize> },
    /// One monitor on the whole target. Nothing to pick out, so no line.
    Solo(usize),
}

impl View {
    fn solo(self) -> Option<usize> {
        match self {
            View::Solo(m) => Some(m),
            View::Bank { .. } => None,
        }
    }
}

pub struct Present {
    pipeline: wgpu::RenderPipeline,
    mark: wgpu::RenderPipeline,
}

/// The focus mark's line, in texels of a 1080-high target: it scales with
/// the target so a 4K display shows the same line, not a hairline.
const MARK: f32 = 2.0;

/// How thick the focus mark is on a target `height` texels high — never
/// under a texel, so a small target still shows one.
pub fn mark_thickness(height: u32) -> f32 {
    (MARK * height as f32 / 1080.0).max(1.0)
}

/// The tile grid for `monitors` tiles: `(columns, rows)`, as square as it
/// can be while every monitor gets a cell.
pub fn grid(monitors: usize) -> (u32, u32) {
    let cols = (monitors as f32).sqrt().ceil() as u32;
    (cols, (monitors as u32).div_ceil(cols))
}

impl Present {
    /// `format` is the target's, which for a surface is the surface's choice
    /// rather than ours.
    pub fn new(device: &wgpu::Device, monitor: &Feedback, format: wgpu::TextureFormat) -> Present {
        let pipeline = crate::fullscreen_pipeline(
            device,
            monitor.shader(),
            monitor.layout(),
            "fs_present",
            format,
            None,
            "present",
        );
        let mark = crate::fullscreen_pipeline(
            device,
            monitor.shader(),
            monitor.layout(),
            "fs_mark",
            format,
            None,
            "focus mark",
        );
        Present { pipeline, mark }
    }

    /// Draws each monitor into the largest centred rectangle of its grid
    /// cell that keeps the monitor's aspect ratio; the rest of the target
    /// stays black. Stretching instead would undo the aspect correction the
    /// sampling transform and the seed spot both go to trouble to maintain.
    ///
    /// A solo is one monitor on the whole target rather than the bank tiled
    /// across it, which is [`tiles`] and nothing else: the same grid with
    /// one tile in it. The overlay, when shown, rides the same pass after
    /// the monitors: it is a caption over the picture, not a second way of
    /// drawing one, and the focus mark is drawn the same way.
    ///
    /// The target is the texture rather than a view and a size, because a
    /// size that is not that texture's puts every viewport somewhere else
    /// and nothing here could tell.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::Texture,
        monitors: &Feedback,
        view: View,
        overlay: Option<&crate::overlay::Overlay>,
    ) {
        let tiles = tiles(monitors.monitors(), view.solo());
        let marked = match view {
            View::Bank { focus } if tiles.len() > 1 => focus,
            _ => None,
        };
        let (cols, rows) = grid(tiles.len());
        let target_size = (target.width(), target.height());
        let cell = (target_size.0 / cols, target_size.1 / rows);
        let target = &target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("present"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Every cell is the same size, so one fit serves them all — and
            // cells too small to hold a viewport skip the lot.
            let fitted = fit(cell, monitors.aspect());
            for (tile, m) in tiles.enumerate() {
                let Some((x, y, width, height)) = fitted else {
                    continue;
                };
                let (col, row) = (tile as u32 % cols, tile as u32 / cols);
                let (x, y) = (x + (col * cell.0) as f32, y + (row * cell.1) as f32);
                pass.set_pipeline(&self.pipeline);
                pass.set_viewport(x, y, width, height, 0.0, 1.0);
                // The dynamic offset picks the monitor: its uniform slot
                // carries its own layer index for fs_present.
                pass.set_bind_group(0, monitors.bind_group(), &[monitors.uniform_offset(m)]);
                pass.draw(0..3, 0..1);
                if marked == Some(m) {
                    pass.set_pipeline(&self.mark);
                    for (sx, sy, sw, sh) in
                        mark_strips((x, y, width, height), mark_thickness(target_size.1))
                    {
                        pass.set_viewport(sx, sy, sw, sh, 0.0, 1.0);
                        pass.draw(0..3, 0..1);
                    }
                }
            }
            if let Some(overlay) = overlay {
                overlay.draw(&mut pass, target_size);
            }
        }
        queue.submit([encoder.finish()]);
    }
}

/// The monitors the display draws, in the order they are tiled: the soloed
/// one alone, or the whole bank.
fn tiles(monitors: usize, solo: Option<usize>) -> std::ops::Range<usize> {
    debug_assert!(solo.is_none_or(|m| m < monitors), "solo of {monitors}");
    solo.map_or(0..monitors, |m| m..m + 1)
}

/// The four edges of `tile`, each `line` texels deep and drawn inside it, so
/// the mark never reaches the neighbouring tile and is never lost under one
/// drawn later. A tile too thin to hold two lines gets none.
pub fn mark_strips(
    tile: (f32, f32, f32, f32),
    line: f32,
) -> impl Iterator<Item = (f32, f32, f32, f32)> {
    let (x, y, w, h) = tile;
    let fits = w >= 2.0 * line && h >= 2.0 * line;
    [
        (x, y, w, line),
        (x, y + h - line, w, line),
        (x, y, line, h),
        (x + w - line, y, line, h),
    ]
    .into_iter()
    .filter(move |_| fits)
}

/// The centred `(x, y, width, height)` of aspect `aspect` inside `target`, or
/// `None` when the target is too small to hold a viewport at all.
fn fit(target: (u32, u32), aspect: f32) -> Option<(f32, f32, f32, f32)> {
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let height = (tw / aspect).min(th);
    let width = height * aspect;
    if width < 1.0 || height < 1.0 {
        return None;
    }
    Some(((tw - width) / 2.0, (th - height) / 2.0, width, height))
}

#[cfg(test)]
mod tests {
    use super::{fit, grid, mark_strips, mark_thickness, tiles};

    #[test]
    fn the_mark_lines_the_inside_of_the_tile_edges() {
        let strips: Vec<_> = mark_strips((10.0, 20.0, 100.0, 50.0), 2.0).collect();
        assert_eq!(
            strips,
            vec![
                (10.0, 20.0, 100.0, 2.0),
                (10.0, 68.0, 100.0, 2.0),
                (10.0, 20.0, 2.0, 50.0),
                (108.0, 20.0, 2.0, 50.0),
            ]
        );
        // Every strip stays inside the tile: a line past its edge lands on
        // the neighbour or outside the target, which wgpu refuses.
        for (x, y, w, h) in strips {
            assert!(x >= 10.0 && x + w <= 110.0 && y >= 20.0 && y + h <= 70.0);
        }
        assert_eq!(mark_strips((0.0, 0.0, 3.0, 50.0), 2.0).count(), 0);
    }

    #[test]
    fn the_mark_scales_with_the_display_and_never_vanishes() {
        assert_eq!(mark_thickness(1080), 2.0);
        assert_eq!(mark_thickness(2160), 4.0);
        assert_eq!(mark_thickness(128), 1.0);
    }

    #[test]
    fn a_matching_target_is_filled_edge_to_edge() {
        assert_eq!(
            fit((1920, 1080), 16.0 / 9.0),
            Some((0.0, 0.0, 1920.0, 1080.0))
        );
    }

    #[test]
    fn a_tall_target_gets_bars_above_and_below() {
        let (x, y, w, h) = fit((1600, 1200), 16.0 / 9.0).unwrap();
        assert_eq!((x, w), (0.0, 1600.0));
        assert!((h - 900.0).abs() < 1e-3, "height {h}");
        assert!((y - 150.0).abs() < 1e-3, "y {y}");
    }

    #[test]
    fn a_wide_target_gets_bars_left_and_right() {
        let (x, y, w, h) = fit((3000, 1080), 16.0 / 9.0).unwrap();
        assert_eq!((y, h), (0.0, 1080.0));
        assert!((w - 1920.0).abs() < 1e-3, "width {w}");
        assert!((x - 540.0).abs() < 1e-3, "x {x}");
    }

    #[test]
    fn a_collapsed_target_draws_nothing() {
        assert_eq!(fit((0, 0), 16.0 / 9.0), None);
        assert_eq!(fit((1, 1000), 16.0 / 9.0), None);
    }

    #[test]
    fn a_solo_is_the_whole_tiling_with_one_tile() {
        // The one test of the solo that needs no adapter: the two that watch
        // the pixels both skip where there is no GPU, and a machine that
        // cannot open one still has to fail a solo that shows the wrong
        // monitor.
        assert_eq!(tiles(4, Some(2)).collect::<Vec<_>>(), vec![2]);
        assert_eq!(grid(tiles(4, Some(2)).len()), (1, 1));
        assert_eq!(tiles(4, None).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!(grid(tiles(4, None).len()), (2, 2));
    }

    #[test]
    fn the_grid_holds_every_monitor_and_stays_square() {
        for monitors in 1..=crate::rig::MONITORS {
            let (cols, rows) = grid(monitors);
            assert!(cols * rows >= monitors as u32, "{monitors} monitors");
            assert!(cols.abs_diff(rows) <= 1, "{monitors}: {cols}x{rows}");
        }
        assert_eq!(grid(1), (1, 1));
        assert_eq!(grid(2), (2, 1));
        assert_eq!(grid(4), (2, 2));
    }
}
