//! The mosaic as SVG: the desktop's paint pass, emitted as markup.
//!
//! The desktop paints tiles and labels into a GPUI canvas; here the same
//! decisions — fills by category and depth, the hatch, the strip, the rings,
//! which labels fit and where the size sits — produce SVG elements instead.
//! Painting order is the desktop's: every fill first, then the outlines, so
//! a directory's children cannot cover its selection ring.

use std::fmt::Write as _;

use disktree_core::classify::Category;
use disktree_core::filter::Keep;
use disktree_core::size::{
    human_bytes, human_bytes_short, human_count, share_bar,
};
use disktree_core::tree::{Metric, Node};
use disktree_core::treemap::{Rect, TileKind};

use crate::app::{Filtered, View, Web};
use crate::palette;
use crate::render::esc;

/// A tile's label, resolved for painting.
pub struct Label {
    pub text: String,
    /// Base-space rectangle; the view transform is applied while painting.
    pub rect: Rect,
    /// The band this label belongs in, when its tile reserved one.
    pub header: Option<Rect>,
    /// Nesting depth in this view: the first level is set in bold.
    pub depth: u32,
    /// Filtered out while typing: drawn quietly.
    pub dim: bool,
    pub marked: bool,
    pub size_text: String,
}

/// What the browser needs for the hover tooltip, resolved at render time so
/// a pointer move costs no round trip.
struct Tip {
    name: String,
    /// The display path, `~`-shortened.
    path: String,
    /// Always bytes: the tooltip's figure, like the desktop's.
    size: String,
    /// `N files · M dirs · X direct`.
    meta: String,
    percent: String,
    /// The 10-cell block bar against the parent's size.
    bar: String,
    dir: bool,
    hidden: bool,
}

/// One painted tile, resolved before emission so emission never looks up.
struct Deco {
    rect: Rect,
    depth: u32,
    category: Category,
    age_bucket: Option<usize>,
    reclaimable: bool,
    filtered: Filtered,
    unreadable: bool,
    marked: bool,
    covered: bool,
    /// The marked directory this tile goes with, by short name.
    covered_by: Option<String>,
    tip: Option<Tip>,
    selected: bool,
}

/// Depth steps a fill distinguishes; deeper clamps.
const DEPTHS: u32 = 5;

/// Two rectangles share any pixels.
fn intersects(a: Rect, b: Rect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

/// The mosaic for the current frame: tiles, labels and the hover tooltip,
/// inside a container the browser measures and reports back the size of.
pub fn mosaic(app: &mut Web) -> String {
    let mut out = String::new();
    let (width, height) = app.mosaic_size;
    let Some(tiles) = app.layout().map(<[_]>::to_vec) else {
        return out;
    };

    // Marks are paths; the mosaic thinks in crumbs. Resolve once per frame
    // rather than building a path for every tile.
    let mut marked: rustc_hash::FxHashSet<Vec<usize>> =
        rustc_hash::FxHashSet::default();
    for item in app.marks.items() {
        if let Some(crumbs) = app.crumbs_for_path(&item.path) {
            marked.insert(crumbs);
        }
    }

    let view = app.view;
    let age = app.color_mode == crate::app::ColorMode::Age;
    let now = app.scanned_at;
    // The DOM is not a GPU: tiles fully outside what the viewport shows —
    // with a quarter-screen margin so small pans stay painted — are never
    // emitted. Hit-testing runs against the full layout, so nothing that
    // can be pointed at is missing.
    let visible = view.visible_base(width, height);
    let keep = visible.inset((-visible.w).mul_add(0.25, -25.0));
    let mut decos = Vec::with_capacity(tiles.len());
    let mut labels = Vec::new();
    for tile in &tiles {
        if !intersects(view.project(tile.rect), keep) {
            continue;
        }
        let crumbs = tile.crumbs();
        let node = match &tile.kind {
            TileKind::Node { crumbs } => app.node_at(crumbs),
            TileKind::Others { .. } => None,
        };
        let category = node.map_or_else(Category::default, |n| n.category);
        let age_bucket =
            (age && node.is_some_and(|n| n.modified > 0)).then(|| {
                let days = (now - node.map_or(now, |n| n.modified)) / 86_400;
                palette::age_bucket(days)
            });
        let filtered = match app.matches.as_deref().map(|m| m.keep(crumbs)) {
            None | Some(Some(Keep::Whole)) => Filtered::Shown,
            Some(Some(Keep::Partial { .. })) => Filtered::Holds,
            Some(None) => Filtered::Out,
        };
        let is_marked = marked.contains(crumbs);
        // Everything inside a marked directory goes with it, so it is drawn
        // marked too.
        let covered_by = if is_marked || marked.is_empty() {
            None
        } else {
            (1..crumbs.len())
                .filter(|length| marked.contains(&crumbs[..*length]))
                .find_map(|length| {
                    let path = app.path_at(&crumbs[..length])?;
                    Some(crate::render::short_name(&path))
                })
        };
        let is_covered = covered_by.is_some();
        let tip = node.map(|node| {
            let parent = &crumbs[..crumbs.len().saturating_sub(1)];
            let parent_value = app.node_at(parent).map_or(0, |p| p.bytes);
            Tip {
                name: node.name.to_string(),
                path: app.path_at(crumbs).map_or_else(String::new, |path| {
                    disktree_core::marks::display_path(
                        &path,
                        app.home.as_deref(),
                    )
                }),
                size: human_bytes(node.bytes),
                meta: format!(
                    "{} files · {} dirs · {} direct",
                    human_count(node.files),
                    human_count(
                        node.dirs.saturating_sub(u64::from(node.is_dir()))
                    ),
                    human_bytes(node.own_bytes),
                ),
                percent: percent(node.bytes, parent_value),
                bar: share_bar(node.bytes, parent_value.max(1), 10),
                dir: node.is_dir(),
                hidden: node.name.starts_with('.'),
            }
        });
        decos.push(Deco {
            rect: tile.rect,
            depth: tile.depth,
            category,
            age_bucket,
            reclaimable: node.is_some_and(|n| n.reclaim.is_some()),
            filtered,
            unreadable: node.is_some_and(|n| n.read_error),
            marked: is_marked,
            covered: is_covered,
            covered_by,
            tip,
            selected: app.selected.as_deref() == Some(crumbs),
        });

        // Labels are chosen in screen space: zooming in makes room for more
        // of them, which is the point of zooming in.
        let screen = view.project(tile.header.unwrap_or(tile.rect));
        if screen.w < crate::app::LABEL_MIN_W
            || screen.h < crate::app::LABEL_MIN_H
        {
            continue;
        }
        match &tile.kind {
            TileKind::Node { crumbs } => {
                let Some(node) = app.node_at(crumbs) else {
                    continue;
                };
                labels.push(Label {
                    text: node.name.to_string(),
                    rect: tile.rect,
                    header: tile.header,
                    depth: tile.depth,
                    dim: filtered == Filtered::Out,
                    marked: is_marked || is_covered,
                    size_text: short_value(node, app.options.metric),
                });
            }
            TileKind::Others { count, .. } => labels.push(Label {
                text: format!("+{count} more"),
                rect: tile.rect,
                header: None,
                depth: tile.depth,
                dim: app.matches.is_some(),
                marked: false,
                size_text: String::new(),
            }),
        }
    }

    labels.sort_by(|left, right| {
        let left_area = left.rect.w * left.rect.h;
        let right_area = right.rect.w * right.rect.h;
        right_area.total_cmp(&left_area)
    });
    labels.truncate(crate::app::MAX_LABELS);

    let _ = write!(
        out,
        "<svg id=\"mosaic\" width=\"{width:.0}\" height=\"{height:.0}\" \
         viewBox=\"0 0 {width:.0} {height:.0}\">"
    );
    // The reclaimable hatch: the desktop's `pattern_slash`, 1 px every 6 px
    // at 45°, quiet enough to leave the hue readable.
    let _ = write!(
        out,
        "<defs><pattern id=\"hatch\" width=\"6\" height=\"6\" \
         patternUnits=\"userSpaceOnUse\" patternTransform=\"rotate(45)\">\
         <rect width=\"1.5\" height=\"6\" fill=\"{}\"/></pattern></defs>",
        palette::css(palette::hatch())
    );

    for tile in &decos {
        let rect = view.project(tile.rect);
        if rect.w <= 0.5 || rect.h <= 0.5 {
            continue;
        }
        // One group per tile: the body carries the hover ring (pure CSS, no
        // round trip) and the tooltip's data; overlays are hit-transparent
        // so the deepest body always wins, the desktop's hit() semantics.
        let class = if tile.selected {
            "g-tile sel"
        } else if tile.marked {
            "g-tile marked"
        } else {
            "g-tile"
        };
        let _ = write!(out, "<g class=\"{class}\"");
        if let Some(tip) = &tile.tip {
            // The hover tooltip travels with the tile: a pointer move then
            // costs the browser nothing but a style recalculation.
            let _ = write!(
                out,
                " data-name=\"{}\" data-tip-path=\"{}\" data-size=\"{}\" \
                 data-meta=\"{}\" data-percent=\"{}\" data-bar=\"{}\"{}{}{}",
                esc(&tip.name),
                esc(&tip.path),
                esc(&tip.size),
                esc(&tip.meta),
                esc(&tip.percent),
                esc(&tip.bar),
                if tip.dir { " data-dir=\"1\"" } else { "" },
                if tip.hidden { " data-hidden=\"1\"" } else { "" },
                if tile.marked {
                    " data-marked=\"1\""
                } else {
                    ""
                },
            );
        }
        if let Some(covered) = &tile.covered_by {
            let _ = write!(out, " data-covered=\"{}\"", esc(covered));
        }
        out.push('>');
        let _ = write!(
            out,
            "<rect class=\"tile-body\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" \
             height=\"{:.1}\" fill=\"{}\"/>",
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            fill(tile)
        );

        // Reclaimable space is hatched, over any hue. Everything inside a
        // reclaimable directory is reclaimable too, so only the outermost one
        // needs painting; its children repaint their own fill and hatch.
        if tile.reclaimable
            && !tile.marked
            && !tile.covered
            && tile.filtered == Filtered::Shown
        {
            let _ = write!(
                out,
                "<rect class=\"overlay\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" \
                 height=\"{:.1}\" fill=\"url(#hatch)\"/>",
                rect.x, rect.y, rect.w, rect.h
            );
        }

        // A top-level directory carries a thin strip of its colour, so the
        // first level of structure reads before any detail.
        if tile.depth == 0
            && tile.age_bucket.is_none()
            && tile.filtered != Filtered::Out
        {
            let _ = write!(
                out,
                "<rect class=\"overlay\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" \
                 height=\"{:.1}\" fill=\"{}\"/>",
                rect.x,
                rect.y,
                rect.w,
                2.0_f32.min(rect.h),
                palette::css(palette::category_accent(tile.category))
            );
        }

        if tile.unreadable && rect.w > 12.0 && rect.h > 12.0 {
            // A small warning corner: part of this was never measured.
            let _ = write!(
                out,
                "<rect class=\"overlay\" x=\"{:.1}\" y=\"{:.1}\" width=\"4\" \
                 height=\"4\" fill=\"{}\"/>",
                rect.x + rect.w - 6.0,
                rect.y + 2.0,
                palette::hex(palette::theme::WARNING, 1.0)
            );
        }
        out.push_str("</g>");
    }

    // Rings after every fill, ranked so the most important is painted last,
    // on top: a directory's children paint over its body, and would
    // otherwise cover its selection ring. The hover ring is the browser's
    // own `:hover`; it never travels.
    let mut rings: Vec<(u8, String)> = Vec::new();
    for tile in &decos {
        let rect = view.project(tile.rect);
        if rect.w <= 0.5 || rect.h <= 0.5 {
            continue;
        }
        let outline = if tile.selected {
            Some((3u8, 2.0f32, palette::css(palette::highlight())))
        } else if tile.marked {
            Some((1, 2.0, palette::hex(palette::theme::DANGER, 1.0)))
        } else {
            None
        };
        if let Some((rank, width, color)) = outline {
            // The stroke straddles the path; shrink the rect by half the
            // width so the ring paints fully inside the tile.
            rings.push((
                rank,
                format!(
                    "<rect class=\"overlay\" x=\"{:.1}\" y=\"{:.1}\" \
                     width=\"{:.1}\" height=\"{:.1}\" fill=\"none\" \
                     stroke=\"{color}\" stroke-width=\"{width}\"/>",
                    rect.x + width / 2.0,
                    rect.y + width / 2.0,
                    (rect.w - width).max(0.0),
                    (rect.h - width).max(0.0),
                ),
            ));
        }
    }
    rings.sort_by_key(|(rank, _)| *rank);
    for (_, ring) in &rings {
        out.push_str(ring);
    }

    paint_labels(&mut out, &labels, view);
    out.push_str("</svg>");
    out
}

/// The fill a tile would get on the desktop, resolved to CSS.
fn fill(tile: &Deco) -> String {
    // Marked, or inside something marked: it all goes together.
    if tile.marked || tile.covered {
        return palette::css(palette::marked_fill());
    }
    let depth = tile.depth.min(DEPTHS - 1);
    let fill = match tile.age_bucket {
        Some(bucket) => palette::age_fill(bucket, depth),
        None => palette::category_fill(tile.category, depth),
    };
    // Only what matches keeps its colour; a directory holding matches steps
    // back less, so the way to them stays readable.
    palette::css(match tile.filtered {
        Filtered::Shown => fill,
        Filtered::Holds => palette::filtered_fill(fill, false),
        Filtered::Out => palette::filtered_fill(fill, true),
    })
}

/// Labels, clipped to the region each owns so no label reaches into another
/// tile. Geometry follows the desktop's paint pass: name at a small inset,
/// the size at the far end of a first-level band, following the name in a
/// deeper one, stacked under it in a closed tile tall enough.
fn paint_labels(out: &mut String, labels: &[Label], view: View) {
    // Type geometry, proportioned to the 12 px label size like the desktop's
    // is to its `name_size`.
    const NAME: f32 = 12.0;
    const SIZE: f32 = 11.0;
    let padding = NAME * 0.42;
    let inset = NAME * 0.25;
    let line = NAME * 1.35;

    for (index, label) in labels.iter().enumerate() {
        let owned = label.header.unwrap_or(label.rect);
        let rect = view.project(owned);
        if rect.w < NAME * 3.3 || rect.h < NAME {
            continue;
        }
        let color = if label.marked {
            palette::hex(palette::theme::DANGER, 1.0)
        } else if label.dim {
            palette::css(palette::Hsl {
                a: 0.5,
                ..palette::label_color(1)
            })
        } else {
            palette::css(palette::label_color(label.depth))
        };
        // The first level is set in bold in its band: it names a region.
        let weight = if label.depth == 0 && label.header.is_some() {
            " font-weight=\"bold\""
        } else {
            ""
        };
        let baseline = NAME.mul_add(0.82, rect.y + inset);
        let _ = write!(
            out,
            "<clipPath id=\"clip-{index}\"><rect x=\"{:.1}\" y=\"{:.1}\" \
             width=\"{:.1}\" height=\"{:.1}\"/></clipPath>",
            rect.x, rect.y, rect.w, rect.h
        );
        let _ = write!(
            out,
            "<g clip-path=\"url(#clip-{index})\"><text x=\"{:.1}\" y=\"{baseline:.1}\" \
             font-size=\"{NAME}\" fill=\"{color}\"{weight}>{}</text>",
            rect.x + padding,
            esc(&label.text),
        );
        if label.size_text.is_empty() {
            out.push_str("</g>");
            continue;
        }
        let dim = palette::css(palette::Hsl {
            a: 0.5,
            ..palette::label_color(1)
        });
        let size = esc(&label.size_text);
        // A closed tile stacks the size under the name when it is tall
        // enough; a first-level band puts it at the far end, where the sizes
        // read as a column; a deeper band follows the name.
        let stacked = label.header.is_none() && rect.h >= line * 2.0 + inset;
        if stacked {
            let _ = write!(
                out,
                "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"{SIZE}\" fill=\"{dim}\">{size}</text>",
                rect.x + padding,
                baseline + line * 0.92,
            );
        } else if label.header.is_some() && label.depth == 0 {
            let _ = write!(
                out,
                "<text x=\"{:.1}\" y=\"{baseline:.1}\" font-size=\"{SIZE}\" fill=\"{dim}\" \
                 text-anchor=\"end\">{size}</text>",
                rect.x + rect.w - padding,
            );
        } else {
            // Following the name needs its width; estimate it and skip the
            // size rather than overlap the next tile's label region.
            let name_w = label.text.chars().count() as f32
                * NAME
                * 0.58
                * if weight.is_empty() { 1.0 } else { 1.06 };
            let room = rect.w - padding * 2.0;
            if room - name_w >= SIZE * 3.0 {
                let _ = write!(
                    out,
                    "<text x=\"{:.1}\" y=\"{baseline:.1}\" font-size=\"{SIZE}\" fill=\"{dim}\">{size}</text>",
                    NAME.mul_add(0.67, rect.x + padding + name_w),
                );
            }
        }
        out.push_str("</g>");
    }
}

/// The value to show for a node under the active metric.
pub fn short_value(node: &Node, metric: Metric) -> String {
    match metric {
        Metric::Bytes => human_bytes_short(node.bytes),
        Metric::Files => human_count(node.files),
    }
}

/// A percentage with one decimal below ten percent.
pub fn percent(part: u64, total: u64) -> String {
    let value = disktree_core::size::share(part, total);
    if value < 9.95 {
        format!("{value:.1}%")
    } else {
        format!("{value:.0}%")
    }
}
