//! The screens as HTML: `disktree-app/src/views.rs` ported to markup.
//!
//! Every screen is a pure function of [`Web`], as the desktop's views are
//! pure functions of `Disktree`. The words, the order and the emphasis are
//! the desktop's; only the box factory changed. Interactive elements carry
//! their event in a `data-ev` attribute, and the browser shim posts it back
//! verbatim — the server keeps every decision.

use std::fmt::Write as _;
use std::path::Path;

use disktree_core::classify::Category;
use disktree_core::insights::Finding;
use disktree_core::removal::{RemovalMode, Target};
use disktree_core::size::{human_bytes, share, share_bar};
use disktree_core::tree::Metric;

use crate::app::{ColorMode, Crumb, Screen, Status, Web};
use crate::mosaic;
use crate::palette;

/// What the browser swaps into the page after any input.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Frame {
    /// The `#app` element's new content.
    pub html: String,
    /// The window title: the directory on screen, as the desktop's is.
    pub title: String,
    /// Scan or removal in flight: the browser polls faster while true.
    pub busy: bool,
    /// The find field's state, so the shim can keep its focus and text.
    pub find_open: bool,
    pub find: String,
    /// Which screen this is, so the shim can route the review screen's own
    /// keys (downloads and the clipboard are client-side things).
    pub screen: &'static str,
}

/// One frame of the whole application.
pub fn frame(app: &mut Web) -> Frame {
    app.pump();
    let body = match app.screen {
        Screen::Explore => explore(app),
        Screen::Review => review(app),
        Screen::Running => running(app),
        Screen::Done => done(app),
    };
    let mut html = body;
    if app.confirm_open {
        html.push_str(&delete_dialog(app));
    }
    if app.show_help {
        html.push_str(&help_overlay(app));
    }
    Frame {
        html,
        screen: match app.screen {
            Screen::Explore => "explore",
            Screen::Review => "review",
            Screen::Running => "running",
            Screen::Done => "done",
        },
        title: format!(
            "disktree · {}",
            disktree_core::marks::display_path(
                &app.current_path(),
                app.home.as_deref()
            )
        ),
        busy: app.busy(),
        find_open: app.find_open,
        find: app.find.clone(),
    }
}

/// HTML-escape text and attribute content.
pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// An event attribute: JSON the browser shim posts back on click.
fn ev(json: &serde_json::Value) -> String {
    format!("data-ev=\"{}\"", esc(&json.to_string()))
}

/// A path for a `data-ev` attribute: its OS bytes, percent-encoded. Names
/// are not necessarily UTF-8 on Unix, and `serde_json`'s answer to a
/// non-UTF-8 `PathBuf` is to panic inside the request handler. Decoded by
/// `server::decode_path` — the codec is owned by this crate on both ends.
pub fn path_attr(path: &Path) -> String {
    let mut out = String::new();
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
        {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn crumbs_json(crumbs: &[usize]) -> serde_json::Value {
    serde_json::Value::from(
        crumbs
            .iter()
            .map(|&index| serde_json::Value::from(index))
            .collect::<Vec<_>>(),
    )
}

// ── explore ─────────────────────────────────────────────────────────────

fn explore(app: &mut Web) -> String {
    let panel = app.show_selection;
    // Git asks about the selection's checkout, as the desktop does once it
    // is visible; the answer is cached per path.
    if panel {
        let target = app.action_target();
        let checkout = target.and_then(|crumbs| {
            let node = app.node_at(&crumbs)?;
            let path = app.path_at(&crumbs)?;
            (node.is_dir() && disktree_core::git::is_checkout(&path))
                .then_some(path)
        });
        if let Some(path) = checkout {
            app.ensure_git(&path);
        }
    }

    let viewport = if app.tree().is_none() {
        scanning_panel(app)
    } else {
        // The tooltip is the browser's own, built from the tiles' data
        // attributes: hover never crosses the wire.
        format!("<div id=\"mosaic-wrap\">{}</div>", mosaic::mosaic(app))
    };

    format!(
        "<div class=\"screen\">{top}<div id=\"explore-body\">\
         <div id=\"main\">{trail_legend}<div id=\"mosaic-outer\">{viewport}</div></div>\
         {panel}</div>{touch}{keys}</div>",
        top = top_bar(app),
        trail_legend = trail_and_legend(app),
        panel = if panel {
            side_panel(app)
        } else {
            String::new()
        },
        touch = touch_bar(app),
        keys = key_bar(app),
    )
}

/// The actions the keys stand in for, as buttons, on touchscreens. Rendered
/// always, shown by the stylesheet only where there is no keyboard; each one
/// sends the key it replaces, so there is exactly one behaviour.
fn touch_bar(app: &Web) -> String {
    let target = app.action_target().unwrap_or_else(|| app.crumbs.clone());
    let node = app.node_at(&target);
    let path = app.path_at(&target);
    let is_root = target == app.crumbs;
    let marked = path.as_deref().is_some_and(|path| app.marks.contains(path));
    let can_open =
        node.is_some_and(disktree_core::tree::Node::is_dir) && !is_root;
    let marks = app.marks.len();
    format!(
        "<div id=\"touch-bar\">\
         <button class=\"btn secondary\" {}>⌫ Up</button>\
         <button class=\"btn highlight\" {} {}>{}</button>\
         <button class=\"btn outline\" {} {}>Open</button>\
         <button class=\"btn secondary\" {}>Filter</button>\
         <button class=\"btn secondary\" id=\"sheet-toggle\">Details</button>\
         <button class=\"btn secondary\" {}>{}{}</button></div>",
        ev(&serde_json::json!({"type": "key", "key": "backspace"})),
        ev(&serde_json::json!({"type": "key", "key": "space"})),
        if is_root { "disabled" } else { "" },
        if marked { "Unmark" } else { "Mark" },
        ev(&serde_json::json!({"type": "key", "key": "enter"})),
        if can_open { "" } else { "disabled" },
        ev(&serde_json::json!({"type": "key", "key": "/"})),
        ev(&serde_json::json!({"type": "key", "key": "c"})),
        if marks > 0 { "Review · " } else { "Review" },
        if marks > 0 {
            format!("<b>{marks}</b>")
        } else {
            String::new()
        },
    )
}

// ── top bar ─────────────────────────────────────────────────────────────

/// The app, what the whole scan found, and the controls that decide what is
/// measured, on the edge they own.
fn top_bar(app: &Web) -> String {
    format!(
        "<div id=\"top-bar\">{logo}{trail}<div class=\"flex-1\"></div>{settings}</div>",
        logo = logo(app.appearance),
        trail = trail(app),
        settings = view_settings(app),
    )
}

/// Four tiles in category colours, and the name.
fn logo(appearance: crate::palette::Appearance) -> String {
    let tiles = [
        Category::Code,
        Category::Git,
        Category::Media,
        Category::Cache,
    ]
    .iter()
    .fold(String::new(), |mut out, &category| {
        let _ = write!(
            out,
            "<span class=\"logo-tile\" style=\"background:{}\"></span>",
            palette::css(palette::category_accent(appearance, category))
        );
        out
    });
    format!(
        "<span id=\"logo\" title=\"disktree\">{tiles}<b>disktree</b></span>"
    )
}

/// The trail, from `/`. Above the scanned root, a crumb widens the scan; in
/// the tree, it goes there, and its ▾ lists its siblings to jump to. A deep
/// trail keeps its first two steps and its last four.
/// Steps a trail shows before it folds its middle into an ellipsis.
const TRAIL_STEPS: usize = 7;

fn trail(app: &Web) -> String {
    let steps = app.breadcrumbs();
    let last = steps.len().saturating_sub(1);
    let widening = app.scan.is_some() && app.scan_root != app.root_path;
    let hidden = if steps.len() > TRAIL_STEPS {
        2..steps.len() - (TRAIL_STEPS - 3)
    } else {
        0..0
    };
    let mut row = String::new();
    for (index, (label, step)) in steps.into_iter().enumerate() {
        if hidden.contains(&index) {
            if index == hidden.start {
                row.push_str(
                    "<span class=\"sep\">/</span><span class=\"fold\">…</span>",
                );
            }
            continue;
        }
        // The root is its own separator: "/" then "home", not "/ / home".
        if index > 1 {
            row.push_str("<span class=\"sep\">/</span>");
        }
        match step {
            Crumb::Above(path) => {
                let pending = widening && app.scan_root == path;
                let _ = write!(
                    row,
                    "<span class=\"crumb above{}\" {} \
                     title=\"Scan from here · what is below is reused\">{}</span>",
                    if pending { " pending" } else { "" },
                    ev(
                        &serde_json::json!({"type": "widen", "path": path_attr(&path)})
                    ),
                    esc(&label),
                );
            }
            Crumb::Tree(crumbs) => {
                row.push_str(&tree_crumb(app, &label, &crumbs, index == last));
            }
        }
    }
    let menu = crumb_menu_layer(app);
    format!("<div id=\"trail\">{row}</div>{menu}")
}

/// A crumb in the tree. Its label goes there (the current one opens the menu
/// instead, being there already); its ▾ opens the sibling menu.
fn tree_crumb(
    app: &Web,
    label: &str,
    crumbs: &[usize],
    current: bool,
) -> String {
    let Some((&index, parent)) = crumbs.split_last() else {
        // The scanned root: no siblings in the tree to offer.
        return format!(
            "<span class=\"crumb{}\" {}>{}</span>",
            if current { " current" } else { "" },
            ev(&serde_json::json!({"type": "goto", "crumbs": []})),
            esc(label),
        );
    };
    let open = app
        .crumb_menu
        .as_ref()
        .is_some_and(|menu| menu.parent == parent && menu.current == index);
    let label_event = if current {
        serde_json::json!({"type": "menu", "crumbs": crumbs})
    } else {
        serde_json::json!({"type": "goto", "crumbs": crumbs})
    };
    format!(
        "<span class=\"crumb-group{}\">\
         <span class=\"crumb{}\" {}>{}</span>\
         <span class=\"crumb chevron\" title=\"siblings\" {}>▾</span></span>",
        if current || open { " lit" } else { "" },
        if current { " current" } else { "" },
        ev(&label_event),
        esc(label),
        ev(&serde_json::json!({"type": "menu", "crumbs": crumbs})),
    )
}

/// The siblings of a crumb, largest first, with a share bar in each one's
/// colour and its size: a sideways jump without going up first.
fn crumb_menu_layer(app: &Web) -> String {
    let Some(menu) = &app.crumb_menu else {
        return String::new();
    };
    let (rows, more) = app.siblings(&menu.parent);
    let largest = rows.first().map_or(1, |row| row.value.max(1));
    let parent_name =
        app.node_at(&menu.parent).map_or_else(String::new, |node| {
            if menu.parent.is_empty() {
                disktree_core::marks::display_path(
                    &app.root_path,
                    app.home.as_deref(),
                )
            } else {
                node.name.to_string()
            }
        });
    let metric = app.options.metric;
    let mut body = String::new();
    for (position, row) in rows.iter().enumerate() {
        let value = match metric {
            Metric::Bytes => human_bytes(row.value),
            Metric::Files => {
                format!("{} files", disktree_core::size::human_count(row.value))
            }
        };
        let _ = write!(
            body,
            "<div class=\"menu-row{}{}\" {}>\
             <span class=\"dot\" style=\"background:{}\"></span>\
             <span class=\"menu-name\">{}</span>\
             <span class=\"bar\"><span style=\"width:{:.0}%;background:{}\"></span></span>\
             <span class=\"menu-value\">{value}</span></div>",
            if row.index == menu.current {
                " current"
            } else {
                ""
            },
            if position == menu.highlighted {
                " highlighted"
            } else {
                ""
            },
            ev(&serde_json::json!({
                "type": "sibling",
                "parent": crumbs_json(&menu.parent),
                "index": row.index,
            })),
            palette::css(palette::category_accent(
                app.appearance,
                row.category
            )),
            esc(&row.name),
            share(row.value, largest).clamp(2.0, 100.0),
            palette::css(palette::category_accent(
                app.appearance,
                row.category
            )),
        );
    }
    if more > 0 {
        let _ = write!(body, "<div class=\"menu-more\">+{more} more</div>");
    }
    let (x, y) = menu.anchor;
    format!(
        "<div id=\"crumb-menu\" style=\"left:{x:.0}px;top:{y:.0}px\">\
         <div class=\"menu-title\">{}</div>{body}</div>",
        esc(&parent_name),
    )
}

/// What is measured, on the edge that owns it: the Size | Files | Age
/// choice, the scan's Hidden and Apparent switches, and the drawn depth.
fn view_settings(app: &Web) -> String {
    // `<` and `>` lead the controls: the directories visited, back and
    // forward, like the browser's own but inside the tree.
    let history_buttons = {
        let button = |back: bool| {
            let (enabled, label, name, arrow) = if back {
                (app.can_go_back(), "‹", "Back", "left")
            } else {
                (app.can_go_forward(), "›", "Forward", "right")
            };
            let keys = format!(
                "alt {} {}",
                if back { "←" } else { "→" },
                name.to_lowercase()
            );
            let target = app.history_target(back).map(|(_, crumbs)| crumbs);
            let (hint, tip) = match &target {
                Some(crumbs) => {
                    let path = app.path_at(crumbs).map(|path| {
                        disktree_core::marks::display_path(
                            &path,
                            app.home.as_deref(),
                        )
                    });
                    (
                        path.as_ref()
                            .map(|path| format!("{name} to {path}"))
                            .unwrap_or_default(),
                        node_tip_data(app, crumbs, &keys),
                    )
                }
                // Nowhere to go: the button is disabled; say what it is for.
                None => (
                    format!("{name} — {keys}"),
                    format!(
                        " data-name=\"{}\" data-note=\"{}\"",
                        esc(name),
                        esc(&keys),
                    ),
                ),
            };
            // The tip lives on the wrapper: a disabled button swallows the
            // pointer events the tooltip needs.
            format!(
                "<span class=\"has-tip\"{}><button class=\"seg\" {} \
                 title=\"{}\" {}>{label}</button></span>",
                tip,
                if enabled { "" } else { "disabled" },
                esc(&hint),
                ev(&serde_json::json!({
                    "type": "key",
                    "key": arrow,
                    "alt": true,
                })),
            )
        };
        format!(
            "<span class=\"group\">{}{}</span>",
            button(true),
            button(false)
        )
    };
    let mode = app.mode_index();
    let mut modes = String::new();
    for (index, label) in ["Size", "Files", "Age"].iter().enumerate() {
        let _ = write!(
            modes,
            "<button class=\"seg{}\" {}>{label}</button>",
            if index == mode { " on" } else { "" },
            ev(&serde_json::json!({"type": "mode", "index": index})),
        );
    }
    let depth = app.layout_options.max_depth;
    format!(
        "<div id=\"settings\">{history_buttons}\
         <span class=\"group\">{modes}</span>\
         <button class=\"check{}\" {} title=\"include dotfiles (i)\">\
         <span class=\"box\"></span>Hidden files</button>\
         <button class=\"check{}\" {} title=\"apparent length, not allocated blocks (d)\">\
         <span class=\"box\"></span>Apparent size</button>\
         <span class=\"group\" title=\"Levels drawn at once · [ and ]\">\
         <span class=\"depth-label\">Depth {depth}</span>\
         <button class=\"seg\" {} {}>−</button>\
         <button class=\"seg\" {} {}>+</button></span></div>",
        if app.options.include_hidden {
            " on"
        } else {
            ""
        },
        ev(&serde_json::json!({"type": "key", "key": "i"})),
        if app.options.apparent_size { " on" } else { "" },
        ev(&serde_json::json!({"type": "key", "key": "d"})),
        if depth <= 1 { "disabled" } else { "" },
        ev(&serde_json::json!({"type": "key", "key": "["})),
        if depth >= 6 { "disabled" } else { "" },
        ev(&serde_json::json!({"type": "key", "key": "]"})),
    )
}

// ── trail and legend ────────────────────────────────────────────────────

/// The scan totals, the find field while it matters, and the colour key.
fn trail_and_legend(app: &Web) -> String {
    let mut row = scan_totals(app);
    if app.find_open || !app.find.is_empty() {
        row.push_str(&find_field(app));
    }
    format!(
        "<div id=\"totals-row\">{row}<div class=\"flex-1\"></div>{}</div>",
        legend(app),
    )
}

/// What the whole scan found, as one quiet line; unreadable paths are
/// flagged in the warning colour when there are any.
fn scan_totals(app: &Web) -> String {
    let tree = app.tree();
    let bytes = tree.map_or(app.progress.bytes, |node| node.bytes);
    let files = tree.map_or(app.progress.files, |node| node.files);
    let dirs = tree.map_or(app.progress.dirs, |node| node.dirs);
    let errors = app.progress.errors;
    let mut line = format!(
        "<span id=\"totals\"><b>{}</b> · {} files · {} dirs",
        human_bytes(bytes),
        disktree_core::size::human_count(files),
        disktree_core::size::human_count(dirs),
    );
    if errors > 0 {
        let _ = write!(
            line,
            " <span class=\"warn\">· {} unreadable</span>",
            disktree_core::size::human_count(errors),
        );
    }
    line.push_str("</span>");
    line
}

/// The key to the colours: the categories, or the age ramp in age mode.
fn legend(app: &Web) -> String {
    let item = |swatch: String, label: &str| {
        format!(
            "<span class=\"legend-item\">{swatch}<span class=\"dim\">{label}</span></span>"
        )
    };
    let swatch = |color: String| {
        format!("<span class=\"swatch\" style=\"background:{color}\"></span>")
    };
    let mut lane = String::new();
    if app.color_mode == ColorMode::Age {
        for (bucket, (_, label)) in palette::AGE_BUCKETS.iter().enumerate() {
            lane.push_str(&item(
                swatch(palette::css(palette::age_accent(
                    app.appearance,
                    bucket,
                ))),
                label,
            ));
        }
    } else {
        for category in Category::LEGEND {
            lane.push_str(&item(
                swatch(palette::css(palette::category_accent(
                    app.appearance,
                    category,
                ))),
                category.label(),
            ));
        }
    }
    let ground = palette::css(palette::category_fill(
        app.appearance,
        Category::Other,
        0,
    ));
    let hatch = format!(
        "<span class=\"swatch hatch-swatch\" style=\"background-color:{ground}\"></span>"
    );
    format!(
        "<span id=\"legend\">{}{lane}</span>",
        item(hatch, "Reclaimable"),
    )
}

fn find_field(app: &Web) -> String {
    // What the text matches, said beside it as it is typed, and what Enter
    // and Escape will do with it.
    let (summary, hint) = match app.matches.as_deref() {
        None => (String::new(), "type to filter"),
        Some(matches) if matches.count == 0 => {
            ("no matches".to_string(), "esc clears")
        }
        Some(matches) => (
            format!(
                "{} match{} · {}",
                disktree_core::size::human_count(matches.count as u64),
                if matches.count == 1 { "" } else { "es" },
                human_bytes(matches.bytes)
            ),
            if app.filter_applied {
                "esc clears"
            } else {
                "enter shows only these"
            },
        ),
    };
    let border = if app.find_open {
        "open"
    } else if app.filter_applied {
        "applied"
    } else {
        ""
    };
    format!(
        "<span id=\"find\" class=\"{border}\">\
         <span class=\"dim\">/</span>\
         <input id=\"find-input\" value=\"{}\" placeholder=\"Filter by name\" \
         autocomplete=\"off\" spellcheck=\"false\"/>\
         <span class=\"caption{}\">{summary}</span>\
         <span class=\"caption dim\">{hint}</span></span>",
        esc(&app.find),
        if app.filter_applied { " hl" } else { " dim" },
    )
}

// ── side panel ──────────────────────────────────────────────────────────

/// Selection, worth a look, marked, and the disk: everything a decision
/// needs, next to the mosaic rather than under it.
fn side_panel(app: &Web) -> String {
    format!(
        "<div id=\"panel\" style=\"width:{:.0}px\">\
         <div id=\"panel-handle\" title=\"drag to resize · double-click resets\"></div>\
         <button id=\"panel-close\" title=\"close\">×</button>\
         {}<div class=\"rule\"></div>\
         <div id=\"panel-lists\">{}<div class=\"rule\"></div>{}</div>\
         {}{}</div>",
        app.panel_px,
        selection_section(app),
        worth_section(app),
        marked_section(app),
        notice_line(app),
        disk_section(app),
    )
}

/// What the keys act on: its name and place, its size set large with its
/// share of the scan, when it was last written, what git says, and the two
/// things to do with it.
fn selection_section(app: &Web) -> String {
    let mut section = String::from("<div class=\"eyebrow\">Selection</div>");
    let target = app.action_target().unwrap_or_else(|| app.crumbs.clone());
    let Some(node) = app.node_at(&target) else {
        section.push_str(
            "<div class=\"dim\">Point at a tile or select one with the arrows</div>",
        );
        return section;
    };
    let path = app.path_at(&target);
    let root_value = app.tree().map_or(0, |tree| tree.bytes);
    let marked = path.as_deref().is_some_and(|path| app.marks.contains(path));
    let covered_by = marks_ancestor(app, path.as_deref());
    let is_current_root = target == app.crumbs;

    let identity = format!(
        "<div class=\"identity\">\
         <div class=\"name-row\"><span class=\"strip\" style=\"background:{}\"></span>\
         <span class=\"name\" title=\"{}\">{}</span></div>\
         <div class=\"caption dim ellipsis\">{}</div></div>",
        palette::css(palette::category_accent(app.appearance, node.category)),
        esc(&node.name),
        esc(&node.name),
        path.as_deref().map_or_else(String::new, |path| {
            esc(&disktree_core::marks::display_path(
                path,
                app.home.as_deref(),
            ))
        }),
    );

    let (number, unit) = match app.options.metric {
        Metric::Bytes => split_size(&human_bytes(node.bytes)),
        Metric::Files => {
            (disktree_core::size::human_count(node.files), "files".into())
        }
    };
    let share = share(node.bytes, root_value);
    let measure = format!(
        "<div class=\"measure\"><span class=\"big\">{number}</span>\
         <span class=\"unit\">{unit}</span></div>\
         <div class=\"bar tall\"><span style=\"width:{share:.1}%\"></span></div>",
    );

    let fourth = if node.is_dir()
        && let Some(path) = &path
        && disktree_core::git::is_checkout(path)
    {
        let (value, clean) = match app.git.get(path) {
            Some(Some(state)) => (state.summary(), state.is_clean()),
            Some(None) => ("not readable".to_string(), false),
            None => ("asking…".to_string(), false),
        };
        figure("Git", &esc(&value), if clean { "ok" } else { "" })
    } else {
        let kind = node.reclaim.map_or_else(
            || node.category.label().to_string(),
            |reason| format!("{} · {}", node.category.label(), reason.label()),
        );
        figure("Kind", &esc(&kind), "")
    };
    let grid_top = format!(
        "{}{}",
        figure("Of scan", &mosaic::percent(node.bytes, root_value), ""),
        figure("Files", &disktree_core::size::human_count(node.files), ""),
    );
    let grid_bottom = format!(
        "{}{}",
        figure(
            "Last write",
            &ago(crate::app::now_seconds(), node.modified),
            ""
        ),
        fourth,
    );
    let grid = format!("<div class=\"grid\">{grid_top}{grid_bottom}</div>");

    // Only states that change the decision earn a badge.
    let mut chips = String::new();
    if marked {
        chips.push_str("<span class=\"chip chip-danger\">Marked</span>");
    }
    if let Some(ancestor) = &covered_by {
        let _ = write!(
            chips,
            "<span class=\"chip chip-danger\">Goes with {}</span>",
            esc(ancestor),
        );
    }
    if node.read_error {
        chips.push_str(
            "<span class=\"chip chip-warn\">Partly unreadable</span>",
        );
    }

    // Open is secondary: Enter already does it. Marking is the action this
    // tool exists for, so it takes the highlight.
    let mut actions = String::new();
    if !is_current_root {
        if node.is_dir() {
            let _ = write!(
                actions,
                "<button class=\"btn outline flex-1\" {}>Open</button>",
                ev(&serde_json::json!({"type": "goto", "crumbs": target})),
            );
        }
        // Inside a marked directory there is nothing to mark on its own: it
        // goes with that directory, so the button offers to keep it by
        // unmarking the directory instead.
        let ancestor =
            path.as_deref().and_then(|path| app.marked_ancestor(path));
        let label = match &ancestor {
            Some(ancestor) if !marked => {
                format!("Unmark {}", short_name(ancestor))
            }
            _ if marked => "Unmark".to_string(),
            _ => "Mark for removal".to_string(),
        };
        let event = match ancestor.as_deref().filter(|_| !marked) {
            Some(ancestor) => {
                serde_json::json!({"type": "unmark", "path": path_attr(ancestor)})
            }
            None => serde_json::json!({"type": "mark", "crumbs": target}),
        };
        let class = if !marked && covered_by.is_none() {
            "btn highlight flex-1"
        } else {
            "btn primary flex-1"
        };
        let _ = write!(
            actions,
            "<button class=\"{class}\" {}>{}</button>",
            ev(&event),
            esc(&label),
        );
    }

    format!(
        "<div class=\"section\">{section}{identity}{measure}{grid}\
         {}<div class=\"actions\">{actions}</div></div>",
        if chips.is_empty() {
            String::new()
        } else {
            format!("<div class=\"chips\">{chips}</div>")
        },
    )
}

/// The biggest things that could plausibly go, with their total.
fn worth_section(app: &Web) -> String {
    let total: u64 = app.insights.iter().map(|candidate| candidate.bytes).sum();
    let largest = app.insights.first().map_or(1, |candidate| candidate.bytes);
    let mut section = String::new();
    let _ = write!(
        section,
        "<div class=\"section-head\"><span class=\"eyebrow\">Worth a look</span>{}</div>",
        if total > 0 {
            format!("<span class=\"caption hl\">{}</span>", human_bytes(total))
        } else {
            String::new()
        },
    );
    if app.insights.is_empty() {
        let _ = write!(
            section,
            "<div class=\"caption dim\">{}</div>",
            if app.tree().is_some() {
                "Nothing obviously disposable"
            } else {
                "Waiting for the scan"
            },
        );
        return section;
    }
    let selected = app.action_target();
    for candidate in &app.insights {
        let Some(node) = app.node_at(&candidate.crumbs) else {
            continue;
        };
        let (title, detail) = insight_text(app, candidate);
        let accent = palette::css(palette::category_accent(
            app.appearance,
            node.category,
        ));
        let active = selected.as_deref() == Some(candidate.crumbs.as_slice());
        let _ = write!(
            section,
            "<div class=\"insight{}\" {} title=\"{}\">\
             <span class=\"strip\" style=\"background:{accent}\"></span>\
             <span class=\"insight-text\"><span class=\"name ellipsis\">{}</span>\
             <span class=\"caption dim ellipsis\">{}</span></span>\
             <span class=\"insight-figures\"><span>{}</span>\
             <span class=\"bar\"><span style=\"width:{:.0}%;background:{accent}\"></span></span></span></div>",
            if active { " active" } else { "" },
            ev(
                &serde_json::json!({"type": "reveal", "crumbs": candidate.crumbs})
            ),
            esc(&title),
            esc(&title),
            esc(&detail),
            human_bytes(candidate.bytes),
            share(candidate.bytes, largest).clamp(2.0, 100.0),
        );
    }
    section
}

/// A finding's title, as the last two parts of its path, and why it is on
/// the list.
fn insight_text(
    app: &Web,
    candidate: &disktree_core::insights::Candidate,
) -> (String, String) {
    let tree = app.tree();
    let chain = tree
        .map_or_else(Vec::new, |tree| tree.resolve_chain(&candidate.crumbs));
    let names: Vec<&str> =
        chain.iter().skip(1).map(|node| &*node.name).collect();
    let tail = names[names.len().saturating_sub(2)..].join("/");
    match &candidate.finding {
        Finding::Reclaimable(reason) => (tail, reason.label().to_string()),
        Finding::Worktrees { count, oldest_days } => (
            tail,
            format!(
                "{count} worktree{} · oldest {oldest_days} d",
                if *count == 1 { "" } else { "s" }
            ),
        ),
        Finding::StaleExperiments { count } => (
            format!("{tail} > {} days", disktree_core::insights::STALE_DAYS),
            format!(
                "{count} experiment{} untouched",
                if *count == 1 { "" } else { "s" }
            ),
        ),
    }
}

/// Marked rows the panel lists before pointing at the review screen.
const MARKED_ROWS: usize = 6;

/// What is queued for removal, each with a way off the list.
fn marked_section(app: &Web) -> String {
    let plan = app.plan();
    let count = app.marks.len();
    let mut section = format!(
        "<div class=\"section-head\"><span class=\"eyebrow\">{}</span>{}</div>",
        if count == 0 {
            "Marked".to_string()
        } else {
            format!("Marked · {count}")
        },
        if count > 0 {
            format!(
                "<span class=\"caption dim\">{}</span>",
                human_bytes(plan.bytes())
            )
        } else {
            String::new()
        },
    );
    if count == 0 {
        section.push_str(
            "<div class=\"caption dim\">Space marks the tile you point at</div>",
        );
        return section;
    }
    for item in app.marks.items().iter().take(MARKED_ROWS) {
        let category = app
            .crumbs_for_path(&item.path)
            .and_then(|crumbs| app.node_at(&crumbs))
            .map_or(Category::Other, |node| node.category);
        let _ = write!(
            section,
            "<div class=\"marked-row\">\
             <span class=\"dot\" style=\"background:{}\"></span>\
             <span class=\"ellipsis flex-1\" title=\"{}\">{}</span>\
             <span class=\"dim\">{}</span>\
             <span class=\"unmark\" {} title=\"unmark\">×</span></div>",
            palette::css(palette::category_accent(app.appearance, category)),
            esc(&item.path.display().to_string()),
            esc(&disktree_core::marks::display_path(
                &item.path,
                app.home.as_deref()
            )),
            human_bytes(item.bytes),
            ev(
                &serde_json::json!({"type": "unmark", "path": path_attr(&item.path)})
            ),
        );
    }
    if count > MARKED_ROWS {
        let _ = write!(
            section,
            "<div class=\"caption dim\">+{} more on the review screen</div>",
            count - MARKED_ROWS,
        );
    }
    if !plan.covered.is_empty() || !plan.blocked.is_empty() {
        let _ = write!(
            section,
            "<div class=\"caption dim\">{} nested · {} kept back</div>",
            plan.covered.len(),
            plan.blocked.len(),
        );
    }
    section
}

/// The last thing that happened, or a scan error, above the disk.
fn notice_line(app: &Web) -> String {
    let (message, class) = if let Some(error) = &app.scan_error {
        (error.clone(), "danger")
    } else {
        let Some((message, status)) = &app.notice else {
            return String::new();
        };
        (
            message.clone(),
            match status {
                Status::Neutral => "neutral",
                Status::Warning => "warn",
            },
        )
    };
    format!("<div class=\"notice {class}\">{}</div>", esc(&message))
}

/// Free space on the volume now, and after the marks go.
fn disk_section(app: &Web) -> String {
    let device = app.device.clone().unwrap_or_default();
    let mut section = format!(
        "<div class=\"section-head\"><span class=\"eyebrow\">Disk</span>\
         <span class=\"caption faint\">{}</span></div>",
        esc(&device),
    );
    let Some(space_info) = app.space else {
        section.push_str(
            "<div class=\"caption dim\">Free space is not available here</div>",
        );
        return format!("<div class=\"section\">{section}</div>");
    };
    let reclaiming = app.plan().bytes();
    let after = space_info.after_removing(reclaiming);
    let (number, unit) = split_size(&human_bytes(space_info.available));
    let total = space_info.total.max(1) as f32;
    let used_now = (space_info.used() as f32 / total).clamp(0.0, 1.0);
    let used_after = (after.used() as f32 / total).clamp(0.0, used_now);

    let _ = write!(
        section,
        "<div class=\"free-row\"><span class=\"big\">{number}</span>\
         <span class=\"unit\">{unit} free</span><span class=\"flex-1\"></span>{}</div>",
        if reclaiming > 0 {
            format!(
                "<span class=\"projection\">→ {} free</span>",
                human_bytes(after.available)
            )
        } else {
            String::new()
        },
    );
    // Used space from the left; the slice the marks give back is the hatched
    // end of it, so the gap it leaves is exactly what comes back.
    let _ = write!(
        section,
        "<div class=\"meter\">\
         <span class=\"used\" style=\"width:{:.1}%\"></span>\
         <span class=\"back\" style=\"left:{:.1}%;width:{:.1}%\"></span></div>",
        used_after * 100.0,
        used_after * 100.0,
        (used_now - used_after) * 100.0,
    );
    let _ = write!(
        section,
        "<div class=\"caption dim spread\"><span>{} used</span><span>{} total</span></div>",
        human_bytes(space_info.used()),
        human_bytes(space_info.total),
    );
    if !app.marks.is_empty() {
        section.push_str(&review_button(app, reclaiming));
    }
    format!("<div class=\"section\">{section}</div>")
}

/// The way to the review screen, where the saving is: it says what is marked
/// and what it frees, so the number that matters is the thing to press.
fn review_button(app: &Web, reclaiming: u64) -> String {
    format!(
        "<button class=\"review-btn\" {}>\
         <span class=\"flex-1 ellipsis\">Review {} marked · frees {}…</span>\
         <span class=\"keycap\">c</span></button>",
        ev(&serde_json::json!({"type": "key", "key": "c"})),
        app.marks.len(),
        human_bytes(reclaiming),
    )
}

// ── key bar ─────────────────────────────────────────────────────────────

/// The keys, quietly: outlines and light labels, there when needed.
fn key_bar(app: &Web) -> String {
    // Most useful first, so a narrow window clips the least useful.
    let hints: [(&str, &str); 10] = [
        ("space", "mark"),
        ("enter", "open"),
        ("⌫", "up"),
        ("c", "review"),
        ("hjkl", "move"),
        ("/", "filter"),
        ("[ ]", "depth"),
        ("t", "mode"),
        ("0", "reset"),
        ("r", "rescan"),
    ];
    let mut lane = String::new();
    for (keys, label) in hints {
        let _ = write!(
            lane,
            "<span class=\"hint\"><span class=\"keycap\">{keys}</span> {label}</span>",
        );
    }
    // Always emitted, empty at 1×, so mosaic-only answers can update it in
    // place without touching the bar around it.
    let zoom = format!(
        "<span id=\"zoom-level\" class=\"caption dim\">{}</span>",
        if (app.view.scale - 1.0).abs() > 0.01 {
            format!("{:.1}×", app.view.scale)
        } else {
            String::new()
        },
    );
    let scan = if app.scan.is_some() {
        format!(
            "scanning · {} entries · {}",
            disktree_core::size::human_count(app.progress.files),
            human_bytes(app.progress.bytes)
        )
    } else {
        let elapsed = app.scan_elapsed.map_or_else(String::new, |time| {
            format!(" · {:.1} s", time.as_secs_f32())
        });
        format!(
            "scan {} entries{elapsed}",
            disktree_core::size::human_count(app.progress.files)
        )
    };
    format!(
        "<div id=\"key-bar\"><div class=\"hints\">{lane}</div>{zoom}\
         <span class=\"hint\" {}><span class=\"keycap\">?</span> all keys</span>\
         <span class=\"caption faint\">{scan}</span></div>",
        ev(&serde_json::json!({"type": "key", "key": "?"})),
    )
}

/// What the viewport shows while the first scan is running.
fn scanning_panel(app: &Web) -> String {
    let progress = &app.progress;
    let stats = format!(
        "{}{}{}{}",
        stat(
            "files",
            &disktree_core::size::human_count(progress.files),
            ""
        ),
        stat(
            "directories",
            &disktree_core::size::human_count(progress.dirs),
            ""
        ),
        stat("measured", &human_bytes(progress.bytes), ""),
        stat(
            "unreadable",
            &disktree_core::size::human_count(progress.errors),
            if progress.errors > 0 { "warn" } else { "dim" },
        ),
    );
    let mut panel = format!(
        "<div id=\"scanning\">\
         <div class=\"title\">Reading {}</div>\
         <div class=\"stats\">{stats}</div>\
         <div class=\"meter wide\"><span class=\"used accent\" style=\"width:{:.0}%\"></span></div>\
         <div class=\"dim\">Marking, zooming and the free-space meter all work as soon as it lands.</div>",
        esc(&disktree_core::marks::display_path(
            &app.root_path,
            app.home.as_deref()
        )),
        progress_estimate(progress.files) * 100.0,
    );
    if let Some(error) = &app.scan_error {
        let _ =
            write!(panel, "<div class=\"notice danger\">{}</div>", esc(error));
    }
    panel.push_str("</div>");
    panel
}

/// A scan has no total to measure against, so the meter shows the work done
/// rather than pretending to know how far along it is.
fn progress_estimate(files: u64) -> f32 {
    if files == 0 {
        0.06
    } else {
        // Asymptotic: the bar keeps moving while the walk continues.
        let scaled = files as f32 / (files as f32 + 20_000.0);
        (0.1 + scaled * 0.85).min(0.99)
    }
}

// ── review ──────────────────────────────────────────────────────────────

/// Rows the review lists before folding the rest into a line.
const LIST_LIMIT: usize = 200;

fn review(app: &Web) -> String {
    let plan = app.plan();
    let items: Vec<Target> = app.marks.items().to_vec();

    let mut list = String::from("<div id=\"marked-list\">");
    if items.is_empty() {
        list.push_str(
            "<div class=\"empty dim\">Nothing is marked. Go back and mark what should go.</div>",
        );
    }
    for item in items.iter().take(LIST_LIMIT) {
        let is_covered =
            plan.covered.iter().any(|covered| covered.path == item.path);
        let blocked_reason = plan
            .blocked
            .iter()
            .find(|blocked| blocked.path == item.path)
            .map(|blocked| blocked.reason.as_str());
        list.push_str(&mark_row(app, item, is_covered, blocked_reason));
    }
    if items.len() > LIST_LIMIT {
        let _ = write!(
            list,
            "<div class=\"caption dim pad\">{} more are marked and will be removed too. \
             Unmark them in the treemap.</div>",
            items.len() - LIST_LIMIT,
        );
    }
    list.push_str("</div>");

    format!(
        "<div class=\"screen\">{}\
         <div class=\"review-body\">{list}{}</div>{}</div>",
        screen_header(
            "Review",
            &format!(
                "{} marked · {} to free",
                app.marks.len(),
                human_bytes(plan.bytes())
            ),
        ),
        review_summary(app, &plan),
        review_footer(app),
    )
}

/// One marked path. Lanes are fixed, so names, bars and sizes line up down
/// the list and sizes can be compared by eye.
fn mark_row(
    app: &Web,
    item: &Target,
    covered: bool,
    blocked: Option<&str>,
) -> String {
    let root_value = app.tree().map_or(0, |tree| tree.bytes);
    let path_text =
        disktree_core::marks::display_path(&item.path, app.home.as_deref());
    let mut chips = String::new();
    if covered {
        chips.push_str(
            "<span class=\"chip chip-dim\">Inside a marked directory</span>",
        );
    }
    if let Some(reason) = &blocked {
        let _ = write!(
            chips,
            "<span class=\"chip chip-warn\">{}</span>",
            esc(reason),
        );
    }
    format!(
        "<div class=\"mark-row{}\">\
         <span class=\"file-icon\">{}</span>\
         <span class=\"mark-name\"><span>{}</span>\
         <span class=\"caption dim ellipsis\">{}</span></span>\
         {chips}\
         <span class=\"share-lane mono\">{}</span>\
         <span class=\"size-lane\">{}</span>\
         <button class=\"btn outline\" {}>Unmark</button></div>",
        if blocked.is_some() || covered {
            " quiet"
        } else {
            ""
        },
        if item.is_dir { "▣" } else { "▢" },
        esc(&short_name(&item.path)),
        esc(&path_text),
        share_bar(item.bytes, root_value.max(1), 8),
        human_bytes(item.bytes),
        ev(
            &serde_json::json!({"type": "unmark", "path": path_attr(&item.path)})
        ),
    )
}

fn review_summary(app: &Web, plan: &disktree_core::removal::Plan) -> String {
    let reclaiming = plan.bytes();
    let trash = app.trash_backend.is_available();

    // One silhouette for one either-or choice, reversible option first.
    let mode = |index: usize, label: &str, disabled: bool| {
        let on =
            usize::from(app.removal_mode == RemovalMode::Permanent) == index;
        format!(
            "<button class=\"seg{}\" {} {}>{label}</button>",
            if on { " on" } else { "" },
            if disabled { "disabled" } else { "" },
            ev(&serde_json::json!({
                "type": "removal",
                "mode": if index == 0 { "trash" } else { "permanent" },
            })),
        )
    };
    let explanation = match app.removal_mode {
        RemovalMode::Trash => format!(
            "Recoverable from the trash until it is emptied. Uses {}.",
            app.trash_backend.label()
        ),
        RemovalMode::Permanent => {
            "Deleted at once, like rm -rf. Nothing is recoverable.".to_string()
        }
    };

    let mut panel = format!(
        "<div id=\"review-summary\">\
         <div class=\"section-label\">What happens</div>\
         <div class=\"group\">{}{}</div>\
         <div class=\"caption dim\">{explanation}</div>\
         <div class=\"section-label\">Totals</div>\
         {}{}{}{}{}",
        mode(0, "Move to trash", !trash),
        mode(1, "Delete permanently", false),
        row("Marked", &app.marks.len().to_string()),
        row("Acted on", &plan.targets.len().to_string()),
        row("Nested, go with a parent", &plan.covered.len().to_string()),
        row("Kept back", &plan.blocked.len().to_string()),
        row("Space freed", &human_bytes(reclaiming)),
    );

    if let Some(volume) = app.space {
        let _ = write!(
            panel,
            "<div class=\"section-label\">Volume</div>{}",
            space_meter(volume, reclaiming),
        );
    }

    if !plan.blocked.is_empty() {
        panel.push_str("<div class=\"section-label\">Kept back</div>");
        for item in plan.blocked.iter().take(6) {
            let _ = write!(
                panel,
                "<div class=\"caption warn\">{}: {}</div>",
                esc(&short_name(&item.path)),
                esc(&item.reason),
            );
        }
    }

    panel.push_str("<div class=\"flex-1\"></div>");
    panel.push_str(&export_controls(plan));
    panel.push_str(&commit_controls(app, plan));
    panel.push_str("</div>");
    panel
}

/// The marked list, handed on instead of acted on: saved as a plain list of
/// paths, or written up as a prompt for a coding agent. The list downloads
/// straight from its route; the prompt is copied by the shim, with a new tab
/// as the fallback where the clipboard API wants a secure context.
fn export_controls(plan: &disktree_core::removal::Plan) -> String {
    let class = if plan.is_empty() {
        "btn secondary flex-1 disabled"
    } else {
        "btn secondary flex-1"
    };
    format!(
        "<div class=\"actions\">\
         <a class=\"{class}\" id=\"export-list\" data-authed \
         href=\"/api/export/list\" download>Save list…</a>\
         <button class=\"{class}\" id=\"copy-prompt\">Copy as prompt</button></div>",
    )
}

/// The screen's one commitment. Moving to the trash is the default commit,
/// so it is the primary action; a permanent deletion is destructive, so it
/// is the danger action and asks first, which its ellipsis promises.
fn commit_controls(app: &Web, plan: &disktree_core::removal::Plan) -> String {
    let count = plan.targets.len();
    let noun = if count == 1 { "item" } else { "items" };
    let (label, class) = match app.removal_mode {
        RemovalMode::Trash => {
            (format!("Move {count} {noun} to trash"), "btn primary")
        }
        RemovalMode::Permanent => {
            (format!("Delete {count} {noun}…"), "btn danger")
        }
    };
    let unavailable = plan.is_empty()
        || (app.removal_mode == RemovalMode::Trash
            && !app.trash_backend.is_available());
    format!(
        "<div class=\"actions end\">\
         <button class=\"btn secondary\" {}>Back</button>\
         <button class=\"{class}\" {} {}>{label}</button></div>",
        ev(&serde_json::json!({"type": "key", "key": "escape"})),
        ev(&serde_json::json!({"type": "commit"})),
        if unavailable { "disabled" } else { "" },
    )
}

fn review_footer(app: &Web) -> String {
    let commit = match app.removal_mode {
        RemovalMode::Trash => "move to trash",
        RemovalMode::Permanent => "delete…",
    };
    format!(
        "<div class=\"footer\">\
         <span class=\"hint\"><span class=\"keycap\">enter</span> {commit}</span>\
         <span class=\"hint\"><span class=\"keycap\">m</span> trash</span>\
         <span class=\"hint\"><span class=\"keycap\">p</span> permanent</span>\
         <span class=\"hint\"><span class=\"keycap\">!</span> unmark all</span>\
         <span class=\"hint\"><span class=\"keycap\">s</span> save list</span>\
         <span class=\"hint\"><span class=\"keycap\">a</span> copy as prompt</span>\
         <span class=\"hint\"><span class=\"keycap\">esc</span> back</span>\
         <div class=\"flex-1\"></div>\
         <span class=\"caption dim\">{} available</span></div>",
        app.space
            .map_or_else(|| "?".into(), |space| human_bytes(space.available)),
    )
}

// ── running ─────────────────────────────────────────────────────────────

fn running(app: &Web) -> String {
    let summary = &app.run_summary;
    let done_count = summary.removed + summary.failed as u64;
    let progress = if summary.total == 0 {
        0.0
    } else {
        done_count as f32 / summary.total as f32
    };

    let mut log = String::from("<div id=\"run-log\">");
    for (path, outcome) in app.run_log.iter().rev().take(200) {
        let ok = outcome.is_ok();
        let error = outcome
            .as_ref()
            .err()
            .map(|error| {
                format!("<span class=\"caption warn\">{}</span>", esc(error))
            })
            .unwrap_or_default();
        let _ = write!(
            log,
            "<div class=\"log-row\"><span class=\"{}\">{}</span>\
             <span class=\"flex-1 ellipsis{}\">{}</span>{error}</div>",
            if ok { "ok" } else { "danger" },
            if ok { "✓" } else { "×" },
            if ok { "" } else { " danger" },
            esc(&short_name(path)),
        );
    }
    log.push_str("</div>");

    format!(
        "<div class=\"screen\">{}<div class=\"run-body\">\
         <div class=\"meter-row\"><span class=\"caption dim\">progress</span>\
         <div class=\"meter\"><span class=\"used{}\" style=\"width:{:.0}%\"></span></div>\
         <span class=\"caption dim\">{} removed · {} to free</span></div>\
         {}{}</div>\
         <div class=\"footer\">\
         <span class=\"hint\"><span class=\"keycap\">esc</span> stop after the current item</span>\
         </div></div>",
        screen_header(
            "Removing",
            &format!("{done_count} of {} done", summary.total)
        ),
        if app.removal_mode == RemovalMode::Permanent {
            " danger-bg"
        } else {
            " accent"
        },
        progress * 100.0,
        summary.removed,
        human_bytes(summary.bytes),
        app.space
            .map_or_else(String::new, |space| space_meter(space, 0)),
        log,
    )
}

// ── done ────────────────────────────────────────────────────────────────

fn done(app: &Web) -> String {
    let summary = &app.run_summary;
    let measured = match (app.space_baseline, app.space) {
        (Some(before), Some(after)) => Some(
            after
                .available
                .cast_signed()
                .saturating_sub(before.available.cast_signed()),
        ),
        _ => None,
    };

    let mut body = format!(
        "<div class=\"run-body\">\
         <div class=\"stats\">{}{}{}{}</div>\
         <div class=\"caption dim\">The treemap is being re-scanned so the numbers on screen match the disk again.</div>",
        stat("removed", &format!("{} items", summary.removed), "ok"),
        stat("bytes claimed", &human_bytes(summary.bytes), ""),
        stat(
            "failed",
            &summary.failed.to_string(),
            if summary.failed > 0 { "danger" } else { "dim" },
        ),
        measured.map_or_else(String::new, |delta| {
            stat(
                "volume freed",
                &if delta >= 0 {
                    format!("+{}", human_bytes(delta.unsigned_abs()))
                } else {
                    format!("-{}", human_bytes(delta.unsigned_abs()))
                },
                if delta >= 0 { "ok" } else { "warn" },
            )
        }),
    );
    if let Some(space) = app.space {
        body.push_str(&space_meter(space, 0));
    }
    if app.scan.is_some() {
        let _ = write!(
            body,
            "<div class=\"meter-row\"><span class=\"caption dim\">re-scanning</span>\
             <div class=\"meter\"><span class=\"used accent\" style=\"width:{:.0}%\"></span></div>\
             <span class=\"caption dim\">{} files</span></div>",
            progress_estimate(app.progress.files) * 100.0,
            disktree_core::size::human_count(app.progress.files),
        );
    }
    if summary.failed > 0 {
        body.push_str(
            "<div class=\"rule\"></div><div class=\"section-label\">what could not be removed</div>",
        );
        for (path, outcome) in app
            .run_log
            .iter()
            .filter(|(_, outcome)| outcome.is_err())
            .take(40)
        {
            let _ = write!(
                body,
                "<div class=\"caption\"><span class=\"danger\">{}</span> \
                 <span class=\"dim\">{}</span></div>",
                esc(&short_name(path)),
                esc(&outcome.as_ref().err().cloned().unwrap_or_default()),
            );
        }
    }
    body.push_str("</div>");

    format!(
        "<div class=\"screen\">{}{}\
         <div class=\"footer\">\
         <button class=\"btn primary\" {}>Done</button>\
         <span class=\"hint\"><span class=\"keycap\">enter</span> continue</span>\
         </div></div>",
        screen_header("Done", "removal finished"),
        body,
        ev(&serde_json::json!({"type": "key", "key": "enter"})),
    )
}

// ── shared ──────────────────────────────────────────────────────────────

fn screen_header(title: &str, subtitle: &str) -> String {
    format!(
        "<div class=\"screen-header\"><span class=\"title\">{title}</span>\
         <span class=\"caption dim\">{}</span></div>",
        esc(subtitle),
    )
}

/// The volume meter: what is used, what is free, and what the marks will
/// free.
fn space_meter(
    space: disktree_core::space::SpaceInfo,
    reclaiming: u64,
) -> String {
    let after = space.after_removing(reclaiming);
    let total = space.total.max(1) as f32;
    let used_now = (space.used() as f32 / total).clamp(0.0, 1.0);
    let used_after = (after.used() as f32 / total).clamp(0.0, used_now);
    format!(
        "<div class=\"space-meter\">\
         <div class=\"meter\"><span class=\"used\" style=\"width:{:.1}%\"></span>\
         <span class=\"back\" style=\"left:{:.1}%;width:{:.1}%\"></span></div>\
         <div class=\"caption dim spread\"><span>{} free{}</span><span>{} total</span></div></div>",
        used_after * 100.0,
        used_after * 100.0,
        (used_now - used_after) * 100.0,
        human_bytes(space.available),
        if reclaiming > 0 {
            format!(" · → {} after", human_bytes(after.available))
        } else {
            String::new()
        },
        human_bytes(space.total),
    )
}

/// The permanent-deletion dialog: it names what goes and what comes back.
/// The trash is reversible and needs no dialog.
fn delete_dialog(app: &Web) -> String {
    let plan = app.plan();
    let title = match plan.targets.as_slice() {
        [only] => format!("Delete “{}” permanently?", short_name(&only.path)),
        targets => format!("Delete {} items permanently?", targets.len()),
    };
    format!(
        "<div class=\"overlay\"><div class=\"dialog\">\
         <div class=\"title\">{}</div>\
         <div class=\"dialog-body\">This frees {}. Deleted files can’t be recovered; \
         move them to the trash if you might need them again.</div>\
         <div class=\"actions end\">\
         <button class=\"btn secondary\" {}>Cancel</button>\
         <button class=\"btn danger\" {}>Delete</button>\
         </div></div></div>",
        esc(&title),
        human_bytes(plan.bytes()),
        ev(&serde_json::json!({"type": "cancel_delete"})),
        ev(&serde_json::json!({"type": "confirm_delete"})),
    )
}

fn help_overlay(app: &Web) -> String {
    // Sentence case, and the tile a key acts on is always the one under the
    // pointer if the pointer moved last, else the keyboard selection.
    let rows: [(&str, &str); 24] = [
        ("space / x", "Mark or unmark the tile you point at"),
        ("ctrl-click", "Mark without moving the selection"),
        ("enter", "Open that directory, at any depth"),
        ("⌫ / esc", "Go up one directory"),
        ("← ↑ ↓ →", "Move between tiles at this level"),
        ("tab", "Next largest sibling"),
        ("scroll", "Zoom toward a directory, then go into it"),
        ("alt ← / →", "Back and forward through visited directories"),
        ("shift-scroll", "Pan the magnified view"),
        ("[ / ]", "Draw fewer or more levels at once"),
        ("- / = / 0", "Magnify, shrink, or reset the view"),
        (
            "ctrl + wheel",
            "The browser's own zoom is the interface zoom",
        ),
        (
            "/",
            "Filter by name: only matches keep their colour; enter shows only them",
        ),
        ("c", "Review the marked list"),
        ("t", "Size, files or age: what areas and colours say"),
        ("r", "Scan again from the same root"),
        ("g", "The whole disk; click any directory above to widen"),
        ("d", "Disk usage or apparent size"),
        ("i", "Include or skip hidden entries"),
        ("p", "Show or hide the selection line"),
        ("", ""),
        (
            "Review screen",
            "m trash · p permanent · ! unmark all · s save list · a copy as prompt",
        ),
        ("", "enter commits · esc goes back"),
        ("", "A permanent deletion always asks first"),
    ];
    let mut keys = String::new();
    for (key, label) in rows {
        if key.is_empty() && label.is_empty() {
            continue;
        }
        let _ = write!(
            keys,
            "<div class=\"help-row\"><span class=\"help-key\">{key}</span>\
             <span>{label}</span></div>",
        );
    }
    format!(
        "<div class=\"overlay\" {}><div class=\"dialog help\">\
         <div class=\"title\">⌨ Keyboard and mouse</div>{keys}\
         <div class=\"caption dim\">? or esc closes · {} · {}</div></div></div>",
        ev(&serde_json::json!({"type": "key", "key": "?"})),
        esc(&app.root_path.display().to_string()),
        app.options.metric.label(),
    )
}

/// The marked ancestor of a path, if any, so the UI can explain nesting.
pub fn marks_ancestor(app: &Web, path: Option<&Path>) -> Option<String> {
    let path = path?;
    app.marks
        .items()
        .iter()
        .filter(|item| item.path != path && path.starts_with(&item.path))
        .map(|item| short_name(&item.path))
        .next()
}

/// A node's card as tooltip data attributes, matching what the mosaic's
/// tiles carry: the shim renders one tooltip shape for both. `keys` says
/// what can be done from there (a tile's is the mark/open line; a history
/// button's is its shortcut).
fn node_tip_data(app: &Web, crumbs: &[usize], keys: &str) -> String {
    let Some(node) = app.node_at(crumbs) else {
        return String::new();
    };
    let path = app.path_at(crumbs);
    let parent = &crumbs[..crumbs.len().saturating_sub(1)];
    let parent_value = app.node_at(parent).map_or(0, |p| p.bytes);
    let mut out = String::new();
    let _ = write!(
        out,
        " data-name=\"{}\" data-tip-path=\"{}\" data-size=\"{}\" \
         data-meta=\"{}\" data-percent=\"{}\" data-bar=\"{}\" data-keys=\"{}\"{}{}",
        esc(&node.name),
        esc(&path.map_or_else(String::new, |path| {
            disktree_core::marks::display_path(&path, app.home.as_deref())
        })),
        esc(&human_bytes(node.bytes)),
        esc(&format!(
            "{} files · {} dirs · {} direct",
            disktree_core::size::human_count(node.files),
            disktree_core::size::human_count(
                node.dirs.saturating_sub(u64::from(node.is_dir()))
            ),
            human_bytes(node.own_bytes),
        )),
        crate::mosaic::percent(node.bytes, parent_value),
        share_bar(node.bytes, parent_value.max(1), 10),
        esc(keys),
        if node.is_dir() { " data-dir=\"1\"" } else { "" },
        if node.name.starts_with('.') {
            " data-hidden=\"1\""
        } else {
            ""
        },
    );
    out
}

pub fn short_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A dim label above a value, for the selection grid.
fn figure(label: &str, value: &str, class: &str) -> String {
    format!(
        "<span class=\"figure\"><span class=\"caption dim\">{label}</span>\
         <span class=\"figure-value {class}\">{value}</span></span>"
    )
}

/// A dim label above a number, for the stat strips.
fn stat(label: &str, value: &str, class: &str) -> String {
    format!(
        "<span class=\"stat {class}\"><span class=\"stat-value\">{value}</span>\
         <span class=\"caption dim\">{label}</span></span>"
    )
}

/// A definition row: label on the left, value on the right.
fn row(label: &str, value: &str) -> String {
    format!(
        "<div class=\"row\"><span class=\"dim\">{label}</span><span>{value}</span></div>"
    )
}

/// A size split into number and unit, so the number can be set large:
/// `90.1 GiB` is `("90.1", "GiB")`.
fn split_size(text: &str) -> (String, String) {
    text.split_once(' ').map_or_else(
        || (text.to_string(), String::new()),
        |(number, unit)| (number.to_string(), unit.to_string()),
    )
}

/// How long ago a Unix time was, in the unit a person would use.
fn ago(now: i64, then: i64) -> String {
    if then <= 0 {
        return "unknown".to_string();
    }
    let seconds = (now - then).max(0);
    let plural = |count: i64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    match seconds {
        0..60 => "just now".to_string(),
        60..3_600 => plural(seconds / 60, "minute"),
        3_600..86_400 => plural(seconds / 3_600, "hour"),
        86_400..2_592_000 => plural(seconds / 86_400, "day"),
        2_592_000..31_536_000 => plural(seconds / 2_592_000, "month"),
        _ => plural(seconds / 31_536_000, "year"),
    }
}
