//! Pure physical-pixel monitor geometry, independent of DPI scale percentages.
use super::Desktop;

#[derive(Clone, Debug)]
pub(crate) struct MonitorLayout {
    pub bounds: Desktop,
    pub monitors: Vec<Desktop>,
    left_edges: Vec<EdgeSpan>,
    right_edges: Vec<EdgeSpan>,
}

#[derive(Clone, Copy, Debug)]
struct EdgeSpan {
    x: i32,
    top: i32,
    bottom: i32,
}

/// Opaque identity of one continuous exposed side, including its orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HandoffSite {
    left: bool,
    x: i32,
    top: i32,
    bottom: i32,
}

const CORNER_MARGIN: i32 = 16;

impl MonitorLayout {
    pub fn new(mut monitors: Vec<Desktop>, fallback: Desktop) -> Self {
        monitors.retain(|m| m.width > 0 && m.height > 0);
        if monitors.is_empty() {
            monitors.push(fallback);
        }
        let left = monitors.iter().map(|m| m.left).min().unwrap();
        let top = monitors.iter().map(|m| m.top).min().unwrap();
        let right = monitors.iter().map(|m| m.right()).max().unwrap();
        let bottom = monitors.iter().map(|m| m.bottom()).max().unwrap();
        let left_edges = Self::edge_spans(&monitors, true);
        let right_edges = Self::edge_spans(&monitors, false);
        Self {
            bounds: Desktop {
                left,
                top,
                width: right - left,
                height: bottom - top,
            },
            monitors,
            left_edges,
            right_edges,
        }
    }

    // Precompute exposed vertical segments once, outside input callbacks. Equal
    // edges across aligned stacked monitors are continuous, not false corners.
    fn edge_spans(monitors: &[Desktop], left: bool) -> Vec<EdgeSpan> {
        let mut boundaries: Vec<_> = monitors.iter().flat_map(|m| [m.top, m.bottom()]).collect();
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut result: Vec<EdgeSpan> = Vec::new();
        for interval in boundaries.windows(2) {
            let (top, bottom) = (interval[0], interval[1]);
            let x = monitors
                .iter()
                .filter(|m| top >= m.top && top < m.bottom())
                .map(|m| if left { m.left } else { m.right() - 1 })
                .reduce(|a, b| if left { a.min(b) } else { a.max(b) });
            let Some(x) = x else { continue };
            if let Some(previous) = result.last_mut() {
                if previous.x == x && previous.bottom == top {
                    previous.bottom = bottom;
                    continue;
                }
            }
            result.push(EdgeSpan { x, top, bottom });
        }
        result
    }

    fn edge_span(&self, y: i32, left: bool) -> Option<&EdgeSpan> {
        let spans = if left {
            &self.left_edges
        } else {
            &self.right_edges
        };
        spans.iter().find(|span| y >= span.top && y < span.bottom)
    }

    /// Inward distance from this row's peer edge, only for an occupied pixel.
    pub fn edge_clearance(&self, x: i32, y: i32, left: bool) -> Option<i32> {
        if !self.monitors.iter().any(|m| m.clamp(x, y) == (x, y)) {
            return None;
        }
        let edge = self.edge_span(y, left)?.x;
        Some(if left {
            x.saturating_sub(edge)
        } else {
            edge.saturating_sub(x)
        })
    }

    /// Deliberate handoff excludes corners/offset shoulders. Occupied monitor
    /// seams with a continuous exposed side do not create artificial dead bands.
    pub fn handoff_edge(&self, x: i32, y: i32, left: bool) -> bool {
        self.handoff_site(x, y, left).is_some()
    }

    pub fn handoff_site(&self, x: i32, y: i32, left: bool) -> Option<HandoffSite> {
        let span = self.edge_span(y, left)?;
        (self.edge_clearance(x, y, left) == Some(0)
            && y.saturating_sub(span.top) >= CORNER_MARGIN
            && (span.bottom - 1).saturating_sub(y) >= CORNER_MARGIN)
            .then_some(HandoffSite {
                left,
                x: span.x,
                top: span.top,
                bottom: span.bottom,
            })
    }

    /// Choose a real monitor pixel, even if the bounding box has a large hole.
    pub fn project(&self, x: i32, y: i32) -> (i32, i32) {
        self.monitors
            .iter()
            .map(|m| {
                let point = m.clamp(x, y);
                let dx = i128::from(x) - i128::from(point.0);
                let dy = i128::from(y) - i128::from(point.1);
                (dx * dx + dy * dy, point)
            })
            .min_by_key(|(distance, _)| *distance)
            .unwrap()
            .1
    }

    /// Map the peer's height onto the exposed edge of a real monitor. Inset keeps
    /// the first reverse delta from immediately returning to the source.
    pub fn entry(&self, left: bool, y: u16, inset: i32) -> (i32, i32) {
        let desired_y = self.bounds.denormalize_y(y);
        let monitor = self
            .monitors
            .iter()
            .min_by_key(|m| {
                let vertical_gap =
                    i64::from(desired_y) - i64::from(desired_y.clamp(m.top, m.bottom() - 1));
                (
                    vertical_gap.abs(),
                    if left {
                        i64::from(m.left)
                    } else {
                        -i64::from(m.right())
                    },
                )
            })
            .unwrap();
        self.project(
            monitor.edge_x(left, inset),
            desired_y.clamp(monitor.top, monitor.bottom() - 1),
        )
    }

    /// Only the exposed row edge switches computers. Monitor seams and holes
    /// inside the virtual bounding box must not be mistaken for the peer edge.
    pub fn on_peer_edge(&self, x: i32, y: i32, left: bool) -> bool {
        self.edge_span(y, left)
            .is_some_and(|span| if left { x <= span.x } else { x >= span.x })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(left: i32, top: i32, width: i32, height: i32) -> Desktop {
        Desktop {
            left,
            top,
            width,
            height,
        }
    }
    fn layout(monitors: Vec<Desktop>) -> MonitorLayout {
        MonitorLayout::new(monitors, rect(0, 0, 1920, 1080))
    }

    #[test]
    fn vertical_layout_center_and_entry_never_land_in_monitor_holes() {
        let screens = layout(vec![rect(0, 0, 1920, 1080), rect(1920, -1440, 2560, 1440)]);
        assert_eq!(screens.project(2240, 200), (2240, -1));
        let left = screens.entry(true, 0, 12);
        assert_eq!(left, (1932, -1440));
        let bottom = screens.entry(false, u16::MAX, 12);
        assert_eq!(bottom, (1907, 1079));
    }

    #[test]
    fn offset_monitor_seams_do_not_switch_to_the_peer() {
        let screens = layout(vec![rect(-1920, 200, 1920, 1080), rect(0, 0, 2560, 1440)]);
        assert!(!screens.on_peer_edge(0, 500, true));
        assert!(screens.on_peer_edge(-1920, 500, true));
        assert!(screens.on_peer_edge(0, 50, true));
        assert!(!screens.on_peer_edge(-1, 500, false));
        assert!(screens.on_peer_edge(2559, 500, false));
    }

    #[test]
    fn negative_origins_and_mixed_pixel_sizes_keep_height_mapping() {
        let screens = layout(vec![rect(-3840, -2160, 3840, 2160), rect(0, 0, 1920, 1080)]);
        assert_eq!(screens.bounds.normalize_y(-2160), 0);
        assert_eq!(screens.bounds.normalize_y(1079), u16::MAX);
        assert_eq!(screens.entry(true, u16::MAX, 12), (12, 1079));
        assert_eq!(screens.entry(false, 0, 12), (-13, -2160));
    }

    #[test]
    fn non_adjacent_monitor_hole_is_projected_to_nearest_valid_pixel() {
        let screens = layout(vec![rect(-1920, 0, 1920, 1080), rect(640, 0, 1920, 1080)]);
        assert_eq!(screens.project(100, 500), (-1, 500));
        assert_eq!(screens.project(600, 500), (640, 500));
        assert!(!screens.on_peer_edge(640, 500, true));
    }

    #[test]
    fn above_main_monitor_edges_use_negative_y_and_the_correct_row() {
        // The top display is offset and narrower than the primary display.
        let screens = layout(vec![rect(0, 0, 2560, 1440), rect(-320, -1080, 1920, 1080)]);
        assert!(screens.on_peer_edge(-320, -500, true));
        assert!(!screens.on_peer_edge(0, -500, true));
        assert!(screens.on_peer_edge(1599, -500, false));
        assert!(screens.on_peer_edge(0, 500, true));
        assert!(screens.on_peer_edge(2559, 500, false));
        assert_eq!(screens.entry(true, 0, 12), (-308, -1080));
        assert_eq!(screens.entry(false, 0, 12), (1587, -1080));
        assert_eq!(screens.entry(true, u16::MAX, 12), (12, 1439));
        let normalized = screens.bounds.normalize_y(-500);
        let (_, mapped_y) = screens.entry(false, normalized, 12);
        assert!((-502..=-500).contains(&mapped_y));
    }
    #[test]
    fn offset_portrait_above_landscape_has_corner_guards_at_each_shoulder() {
        let screens = layout(vec![rect(0, 0, 2400, 1200), rect(900, -1800, 1000, 1800)]);
        assert!(screens.handoff_edge(900, -900, true));
        assert!(screens.handoff_edge(1899, -900, false));
        assert!(!screens.handoff_edge(900, -1, true));
        assert!(!screens.handoff_edge(0, 0, true));
        assert!(!screens.handoff_edge(1899, -1, false));
        assert!(!screens.handoff_edge(2399, 0, false));
        assert!(screens.handoff_edge(0, 300, true));
        assert!(screens.handoff_edge(2399, 300, false));
        assert!(!screens.handoff_edge(0, -900, true));
        assert_eq!(screens.edge_clearance(912, -900, true), Some(12));
    }

    #[test]
    fn aligned_stacked_sides_are_continuous_while_holes_are_not_handoff_sites() {
        let screens = layout(vec![
            rect(-1600, -900, 1600, 900),
            rect(-1600, 0, 1600, 1000),
        ]);
        assert!(screens.handoff_edge(-1600, -1, true));
        assert!(screens.handoff_edge(-1600, 0, true));
        assert!(screens.handoff_edge(-1, 0, false));
        assert!(!screens.handoff_edge(-1600, -900, true));
        let hole = layout(vec![rect(-1600, 0, 1600, 900), rect(500, -900, 1000, 900)]);
        assert_eq!(hole.edge_clearance(0, -500, true), None);
        assert!(!hole.handoff_edge(0, -500, true));
    }
}
