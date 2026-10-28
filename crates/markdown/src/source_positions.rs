use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Bounds, Pixels, ScrollHandle, px};

use crate::{RenderedLine, RenderedText};

/// Where a Markdown element laid out its source, recorded on every prepaint so a view can line up
/// a scroll position with an offset in the Markdown source.
///
/// Positions are measured from the top of the scrolled content when the element is given a
/// scroll handle, and from the top of the element otherwise.
#[derive(Clone, Default)]
pub struct SourcePositions(Rc<RefCell<Vec<SourcePosition>>>);

#[derive(Clone, Copy, Debug, PartialEq)]
struct SourcePosition {
    source_offset: f64,
    top: f64,
}

impl SourcePositions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// The vertical position of a source offset, interpolated between the rendered lines around
    /// it. Fractional offsets land between characters, so a scroll position can move smoothly.
    pub fn top_for_source_offset(&self, source_offset: f64) -> Option<Pixels> {
        let positions = self.0.borrow();
        interpolate(
            &positions,
            source_offset,
            |position| position.source_offset,
            |position| position.top,
        )
        .map(|top| px(top as f32))
    }

    /// The source offset laid out at a vertical position, interpolated between the rendered lines
    /// around it.
    pub fn source_offset_for_top(&self, top: Pixels) -> Option<f64> {
        let positions = self.0.borrow();
        interpolate(
            &positions,
            f64::from(top),
            |position| position.top,
            |position| position.source_offset,
        )
    }

    pub(crate) fn record(
        &self,
        text: &RenderedText,
        source_len: usize,
        element_bounds: Bounds<Pixels>,
        scroll_handle: Option<&ScrollHandle>,
    ) {
        let content_top = match scroll_handle {
            Some(scroll_handle) => scroll_handle.bounds().top() + scroll_handle.offset().y,
            None => element_bounds.top(),
        };
        let rows = text
            .lines
            .iter()
            .flat_map(|line| rows_of_line(line, content_top));
        *self.0.borrow_mut() =
            positions_for_rows(rows, source_len, element_bounds.bottom() - content_top);
    }
}

/// A visual row of rendered text and the source it was rendered from.
struct RowExtent {
    source_start: usize,
    /// `None` for a source anchor, a one-line placeholder at the top of an element like a Mermaid
    /// diagram, whose source runs on to the line after the element.
    source_end: Option<usize>,
    top: Pixels,
    bottom: Pixels,
}

/// One rendered line can hold a whole code block or a paragraph wrapped over several rows, whose
/// rows hold different amounts of source.
fn rows_of_line(line: &RenderedLine, content_top: Pixels) -> Vec<RowExtent> {
    let Some(first_mapping) = line.source_mappings.first() else {
        return Vec::new();
    };
    let bounds = line.layout.bounds();
    if line.is_source_anchor {
        return vec![RowExtent {
            source_start: first_mapping.source_index,
            source_end: None,
            top: bounds.top() - content_top,
            bottom: bounds.bottom() - content_top,
        }];
    }
    let line_height = line.layout.line_height();
    let mut rows = Vec::new();
    let mut top = bounds.top() - content_top;
    let mut hard_line_start = 0;
    for layout in line.layout.line_layouts() {
        let hard_line_end = hard_line_start + layout.len();
        // Each wrap boundary names the glyph that starts the next row.
        let wrapped_row_starts = layout.wrap_boundaries().iter().filter_map(|boundary| {
            let glyph = layout
                .runs()
                .get(boundary.run_ix)?
                .glyphs
                .get(boundary.glyph_ix)?;
            Some(hard_line_start + glyph.index)
        });
        let row_starts: Vec<usize> = std::iter::once(hard_line_start)
            .chain(wrapped_row_starts)
            .collect();
        let row_ends = row_starts.iter().skip(1).copied().chain([hard_line_end]);
        for (row_start, row_end) in row_starts.iter().copied().zip(row_ends) {
            rows.push(RowExtent {
                source_start: line.source_index_for_rendered_index(row_start),
                source_end: Some(line.source_index_for_rendered_index(row_end)),
                top,
                bottom: top + line_height,
            });
            top += line_height;
        }
        // Rendered lines are joined by newlines.
        hard_line_start = hard_line_end + 1;
    }
    rows
}

fn positions_for_rows(
    rows: impl IntoIterator<Item = RowExtent>,
    source_len: usize,
    content_bottom: Pixels,
) -> Vec<SourcePosition> {
    // The source's start maps to the content's top, so a view scrolled to the top of its source
    // shows the content's top padding too.
    let mut positions = vec![SourcePosition {
        source_offset: 0.,
        top: 0.,
    }];
    for row in rows {
        push_in_order(&mut positions, row.source_start, row.top);
        if let Some(source_end) = row.source_end {
            push_in_order(&mut positions, source_end, row.bottom);
        }
    }
    // Images and diagrams have no text lines, so the content's bottom anchors anything rendered
    // after the last one.
    push_in_order(&mut positions, source_len, content_bottom);
    positions
}

/// Keeps positions ascending in both source offset and top, so they can be interpolated either
/// way. Lines that break the order, like table cells side by side, are left out.
fn push_in_order(positions: &mut Vec<SourcePosition>, source_offset: usize, top: Pixels) {
    let position = SourcePosition {
        source_offset: source_offset as f64,
        top: f64::from(top),
    };
    let in_order = positions
        .last()
        .is_none_or(|last| position.source_offset > last.source_offset && position.top >= last.top);
    if in_order {
        positions.push(position);
    }
}

fn interpolate(
    positions: &[SourcePosition],
    key: f64,
    key_of: impl Fn(&SourcePosition) -> f64,
    value_of: impl Fn(&SourcePosition) -> f64,
) -> Option<f64> {
    let next_index = positions.partition_point(|position| key_of(position) <= key);
    let Some(previous) = next_index.checked_sub(1).and_then(|ix| positions.get(ix)) else {
        return positions.first().map(&value_of);
    };
    let Some(next) = positions.get(next_index) else {
        return Some(value_of(previous));
    };
    let span = key_of(next) - key_of(previous);
    if span <= 0. {
        return Some(value_of(previous));
    }
    let fraction = (key - key_of(previous)) / span;
    Some(value_of(previous) + fraction * (value_of(next) - value_of(previous)))
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    fn positions(points: &[(f64, f64)]) -> SourcePositions {
        SourcePositions(Rc::new(RefCell::new(
            points
                .iter()
                .map(|&(source_offset, top)| SourcePosition { source_offset, top })
                .collect(),
        )))
    }

    #[test]
    fn interpolates_between_rendered_lines_both_ways() {
        let positions = positions(&[(0., 0.), (10., 100.), (20., 400.)]);

        assert_eq!(positions.top_for_source_offset(5.), Some(px(50.)));
        assert_eq!(positions.top_for_source_offset(15.), Some(px(250.)));
        assert_eq!(positions.source_offset_for_top(px(250.)), Some(15.));
        assert_eq!(positions.source_offset_for_top(px(100.)), Some(10.));
    }

    #[test]
    fn clamps_outside_the_recorded_range() {
        let positions = positions(&[(0., 0.), (10., 100.)]);

        assert_eq!(positions.top_for_source_offset(-1.), Some(px(0.)));
        assert_eq!(positions.top_for_source_offset(50.), Some(px(100.)));
        assert_eq!(positions.source_offset_for_top(px(500.)), Some(10.));
        assert_eq!(SourcePositions::new().top_for_source_offset(1.), None);
    }

    #[test]
    fn source_anchors_span_down_to_the_next_line() {
        let row = |source: Range<usize>, top: f32, bottom: f32, is_source_anchor: bool| RowExtent {
            source_start: source.start,
            source_end: (!is_source_anchor).then_some(source.end),
            top: px(top),
            bottom: px(bottom),
        };
        // A diagram 400px tall whose anchor line is only 20px tall.
        let positions = SourcePositions(Rc::new(RefCell::new(positions_for_rows(
            [
                row(0..10, 0., 20., false),
                row(12..50, 40., 60., true),
                row(52..60, 460., 480., false),
            ],
            60,
            px(480.),
        ))));

        assert_eq!(positions.top_for_source_offset(12.), Some(px(40.)));
        assert_eq!(positions.top_for_source_offset(32.), Some(px(250.)));
        assert_eq!(positions.source_offset_for_top(px(250.)), Some(32.));
    }

    #[gpui::test]
    fn code_block_rows_start_at_their_own_source_lines(cx: &mut gpui::TestAppContext) {
        let source = "```\nthe first line is much longer than the others\nb\nc\n```\n";
        let rendered = crate::tests::render_markdown(source, cx);
        assert_eq!(
            rendered.lines.len(),
            1,
            "a code block is laid out as one text"
        );
        let rows: Vec<_> = rendered
            .lines
            .iter()
            .flat_map(|line| rows_of_line(line, Pixels::ZERO))
            .collect();

        let line_starts: Vec<usize> = ["the first", "b\n", "c\n"]
            .into_iter()
            .filter_map(|line| source.find(line))
            .collect();
        let row_starts: Vec<usize> = rows.iter().map(|row| row.source_start).collect();
        assert_eq!(row_starts, line_starts);
        let row_tops: Vec<Pixels> = rows.iter().map(|row| row.top - rows[0].top).collect();
        let line_height = rows[0].bottom - rows[0].top;
        assert_eq!(row_tops, vec![Pixels::ZERO, line_height, line_height * 2.]);
    }

    #[test]
    fn skips_positions_that_break_the_order() {
        let mut points = Vec::new();
        push_in_order(&mut points, 0, px(0.));
        push_in_order(&mut points, 10, px(100.));
        push_in_order(&mut points, 20, px(50.));
        push_in_order(&mut points, 10, px(200.));
        push_in_order(&mut points, 30, px(100.));

        let offsets: Vec<_> = points.iter().map(|point| point.source_offset).collect();
        assert_eq!(offsets, vec![0., 10., 30.]);
    }
}
