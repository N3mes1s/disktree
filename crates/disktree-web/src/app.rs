//! Application state and every mutation the web UI can perform.
//!
//! This is `disktree-app/src/state.rs` with the GPUI wiring taken out: the
//! same fields, the same actions, the same order of decisions. What the
//! desktop does with `cx.notify()` this does by answering the next frame
//! request; what it does with background executors and timers this does in
//! [`Web::pump`], called on every request, because the browser polls while
//! anything runs. The design rule of the desktop stands: the HTML in
//! [`crate::render`] is a pure function of this state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use disktree_core::classify::Category;
use disktree_core::filter::{Keep, Matches, filter};
use disktree_core::insights::{Candidate, worth_a_look};
use disktree_core::marks::{Marks, display_path, is_hidden};
use disktree_core::removal::{
    Plan, RemovalEvent, RemovalHandle, RemovalMode, Target, TrashBackend,
    detect_trash_backend, plan,
};
use disktree_core::scan::{Known, ScanHandle, ScanOptions, ScanSnapshot};
use disktree_core::space::{
    SpaceInfo, device_for, space_info, volume_root_for,
};
use disktree_core::tree::{Metric, Node, path_of};
use disktree_core::treemap::{LayoutOptions, Rect, Tile, hit, layout_filtered};

use disktree_core::git::GitState;

/// What a tile's colour says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// The kind of data it is.
    #[default]
    Kind,
    /// How long since anything in it was written.
    Age,
}

/// A trail crumb's sibling menu, open.
#[derive(Clone, Debug, PartialEq)]
pub struct CrumbMenu {
    /// Crumbs of the directory whose children are listed.
    pub parent: Vec<usize>,
    /// The listed child the crumb stands for.
    pub current: usize,
    /// The row the arrow keys are on, as an index into [`Web::siblings`].
    pub highlighted: usize,
    /// Where the browser drew the crumb, so the menu opens beside it.
    pub anchor: (f32, f32),
}

/// One row of a sibling menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sibling {
    /// Index among the parent's children.
    pub index: usize,
    pub name: String,
    pub value: u64,
    pub category: Category,
    pub is_dir: bool,
}

/// Rows a sibling menu lists; the rest are counted.
pub const SIBLING_ROWS: usize = 24;

/// One step of the trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Crumb {
    /// A directory above the scanned root: going there scans only what is
    /// new at that level.
    Above(PathBuf),
    /// A directory in the tree, by crumbs from the scanned root.
    Tree(Vec<usize>),
}

/// Which screen the app is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// Walk the treemap and mark what should go.
    #[default]
    Explore,
    /// Review every marked path and choose how to remove them.
    Review,
    /// A removal is running.
    Running,
    /// The removal finished; show what happened.
    Done,
}

/// The treemap view transform: `screen = (base - origin) * scale`.
///
/// All viewport geometry stays in the base space of an unzoomed layout, so pan
/// and zoom never require a re-layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub scale: f32,
    pub origin_x: f32,
    pub origin_y: f32,
}

impl Default for View {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl View {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        origin_x: 0.0,
        origin_y: 0.0,
    };
    pub const MIN_SCALE: f32 = 1.0;
    /// Past this, zooming again descends into whatever is under the cursor
    /// instead of magnifying further: the continuous "keep zooming and you
    /// are inside" gesture, with a breadcrumb to walk back out.
    pub const MAX_SCALE: f32 = 5.0;

    pub fn project(&self, rect: Rect) -> Rect {
        Rect::new(
            (rect.x - self.origin_x) * self.scale,
            (rect.y - self.origin_y) * self.scale,
            rect.w * self.scale,
            rect.h * self.scale,
        )
    }

    /// Viewport point to base-space point.
    pub fn unproject(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x / self.scale + self.origin_x,
            y / self.scale + self.origin_y,
        )
    }

    /// Zoom by `factor` toward `(x, y)` and no further than `ceiling`.
    pub fn zoomed_at(&self, x: f32, y: f32, factor: f32, ceiling: f32) -> Self {
        let scale = (self.scale * factor).clamp(Self::MIN_SCALE, ceiling);
        let (base_x, base_y) = self.unproject(x, y);
        Self {
            scale,
            origin_x: base_x - x / scale,
            origin_y: base_y - y / scale,
        }
    }

    /// The scale at which `rect` exactly fits the viewport.
    pub fn fit_scale(rect: Rect, width: f32, height: f32) -> f32 {
        let width = width.max(1.0);
        let height = height.max(1.0);
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return Self::MIN_SCALE;
        }
        (width / rect.w).min(height / rect.h)
    }

    /// The region of the base layout the viewport currently shows.
    pub fn visible_base(&self, width: f32, height: f32) -> Rect {
        Rect::new(
            self.origin_x,
            self.origin_y,
            width.max(1.0) / self.scale,
            height.max(1.0) / self.scale,
        )
    }

    /// Keep the viewport inside the layout: no empty margins, ever.
    #[must_use]
    pub fn clamped(self, width: f32, height: f32) -> Self {
        let max_x = (width - width / self.scale).max(0.0);
        let max_y = (height - height / self.scale).max(0.0);
        Self {
            scale: self.scale,
            origin_x: self.origin_x.clamp(0.0, max_x),
            origin_y: self.origin_y.clamp(0.0, max_y),
        }
    }
}

/// How a tile stands against the find text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filtered {
    /// It matches, is inside a match, or nothing is being found.
    Shown,
    /// It holds matches: its fill steps back, its name stays readable.
    Holds,
    /// Nothing in it matches.
    Out,
}

/// Layout for one (crumbs, area, options) combination.
struct LayoutCache {
    key: LayoutKey,
    tiles: Vec<Tile>,
}

#[derive(Clone, PartialEq)]
struct LayoutKey {
    crumbs: Vec<usize>,
    width: f32,
    height: f32,
    options: LayoutOptions,
    /// Bumped whenever the applied filter changes.
    filter: u64,
}

/// Result of the removal run, summarised for the final screen.
#[derive(Clone, Debug, Default)]
pub struct RunSummary {
    pub total: usize,
    pub removed: u64,
    pub bytes: u64,
    pub failed: usize,
}

/// A notice's weight, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Neutral,
    Warning,
}

/// How many "worth a look" findings the panel lists.
const INSIGHT_LIMIT: usize = 6;

/// How many labels one frame emits.
pub const MAX_LABELS: usize = 150;

/// Height of a top-level directory's name band, in px at the fixed 16 px rem.
pub const HEADER_PX: f32 = 22.0;
/// The slim label row a deeper open directory keeps.
pub const HEADER_INNER_PX: f32 = 16.0;

/// Smallest tile, in px, that gets a label: below this a name cannot be read.
pub const LABEL_MIN_W: f32 = 54.0;
pub const LABEL_MIN_H: f32 = 15.0;

/// The web rem: the browser's default, which its zoom scales for us.
pub const REM: f32 = 16.0;

/// The side panel's width limits: the desktop's 17–44 rem, in px.
pub const PANEL_MIN_PX: f32 = 17.0 * REM;
pub const PANEL_MAX_PX: f32 = 44.0 * REM;
pub const PANEL_DEFAULT_PX: f32 = 23.0 * REM;

/// A trail step's label: the directory's own name, or `/` for the root.
pub fn crumb_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Now, in Unix seconds.
pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Everything the app knows and everything it can do.
pub struct Web {
    /// The directory the tree was scanned from.
    pub root_path: PathBuf,
    pub home: Option<PathBuf>,
    pub options: ScanOptions,
    pub tree: Option<Arc<Node>>,
    pub scan: Option<ScanHandle>,
    pub progress: ScanSnapshot,
    pub scan_error: Option<String>,

    pub screen: Screen,
    /// Path from the scanned root to the directory currently drawn.
    pub crumbs: Vec<usize>,
    pub selected: Option<Vec<usize>>,
    pub hovered: Option<Vec<usize>>,
    /// The pointer moved more recently than the keyboard navigated. Then the
    /// tile under the pointer is what Space, X and Enter act on; after an
    /// arrow or Tab it is the keyboard selection again.
    pub pointer_active: bool,
    pub view: View,
    pub layout_options: LayoutOptions,
    cache: Option<LayoutCache>,

    /// Mouse position in mosaic-local pixels, for the tooltip.
    pub pointer: Option<(f32, f32)>,
    /// The mosaic's size in CSS pixels, reported by the browser with every
    /// request: layout runs in these pixels.
    pub mosaic_size: (f32, f32),

    pub marks: Marks,
    pub removal_mode: RemovalMode,
    pub trash_backend: TrashBackend,
    /// The permanent-deletion confirmation is open. Trash needs no dialog:
    /// it is reversible, so it commits directly.
    pub confirm_open: bool,
    pub run: Option<RemovalHandle>,
    pub run_summary: RunSummary,
    pub run_log: Vec<(PathBuf, Result<(), String>)>,
    pub space: Option<SpaceInfo>,
    /// Free space before the removal, for the honest "what did it actually
    /// free" number rather than the sum of what was marked.
    pub space_baseline: Option<SpaceInfo>,
    space_checked: Instant,
    pub notice: Option<(String, Status)>,

    pub find: String,
    /// The find field has the browser's focus.
    pub find_open: bool,
    /// What the find text matches in the directory on screen, recomputed on
    /// every keystroke. While typing it only dims what does not match.
    pub matches: Option<Arc<Matches>>,
    /// Enter was pressed: the mosaic lays out only the matches.
    pub filter_applied: bool,
    filter_epoch: u64,
    pub show_help: bool,
    pub show_selection: bool,
    /// A trail crumb's sibling menu, when open.
    pub crumb_menu: Option<CrumbMenu>,

    pub color_mode: ColorMode,
    /// The largest things worth clearing, recomputed when a scan lands.
    pub insights: Vec<Candidate>,
    /// What git knows about each checkout that has been selected; `None`
    /// once asked and found not to be one.
    pub git: std::collections::HashMap<PathBuf, Option<GitState>>,
    /// The device the scanned volume is mounted from.
    pub device: Option<String>,
    /// The top of the disk the home directory lives on: what "Whole disk"
    /// scans.
    pub disk_root: Option<PathBuf>,
    /// The side panel's width, in px; dragged from its left edge.
    pub panel_px: f32,
    pub scan_started: Option<Instant>,
    /// Where the scan in flight is rooted. Differs from `root_path` while
    /// widening, when the old tree stays on screen until the wider tree lands.
    pub scan_root: PathBuf,
    pub scan_elapsed: Option<Duration>,
    /// Unix seconds when the tree landed: the "now" ages are measured from.
    pub scanned_at: i64,
}

impl Web {
    pub fn new(root_path: PathBuf, options: ScanOptions, depth: u32) -> Self {
        let mut app = Self::base(root_path, options, depth);
        app.start_scan();
        app
    }

    /// The constructor's state with nothing started; `new` adds the scan.
    fn base(root_path: PathBuf, options: ScanOptions, depth: u32) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let space = space_info(&root_path).ok();
        let trash_backend = detect_trash_backend();
        let mut app = Self {
            root_path,
            home,
            options,
            tree: None,
            scan: None,
            progress: ScanSnapshot::default(),
            scan_error: None,
            screen: Screen::Explore,
            crumbs: Vec::new(),
            selected: None,
            hovered: None,
            pointer_active: false,
            view: View::default(),
            layout_options: LayoutOptions {
                max_depth: depth.clamp(1, 6),
                ..LayoutOptions::default()
            },
            cache: None,
            pointer: None,
            mosaic_size: (0.0, 0.0),
            marks: Marks::default(),
            // Reversible by default whenever this machine has a trash: the
            // permanent path stays one choice away, behind a dialog.
            removal_mode: if trash_backend.is_available() {
                RemovalMode::Trash
            } else {
                RemovalMode::Permanent
            },
            trash_backend,
            confirm_open: false,
            run: None,
            run_summary: RunSummary::default(),
            run_log: Vec::new(),
            space,
            space_baseline: None,
            space_checked: Instant::now(),
            notice: None,
            find: String::new(),
            find_open: false,
            matches: None,
            filter_applied: false,
            filter_epoch: 0,
            show_help: false,
            show_selection: true,
            crumb_menu: None,
            color_mode: ColorMode::Kind,
            insights: Vec::new(),
            git: std::collections::HashMap::new(),
            device: None,
            disk_root: None,
            panel_px: PANEL_DEFAULT_PX,
            scan_started: None,
            scan_root: PathBuf::new(),
            scan_elapsed: None,
            scanned_at: now_seconds(),
        };
        app.device = device_for(&app.root_path);
        app.disk_root = app
            .home
            .as_deref()
            .or(Some(app.root_path.as_path()))
            .and_then(volume_root_for);
        app
    }

    /// Build a view over a tree that is already known.
    ///
    /// Mirrors [`Self::new`] without starting a walk, so a test can hand the
    /// screens a tree it already has instead of waiting on a real one.
    #[cfg(test)]
    pub fn with_tree(
        root_path: PathBuf,
        tree: Node,
        options: ScanOptions,
        depth: u32,
    ) -> Self {
        let mut app = Self::base(root_path, options, depth);
        app.marks.refresh(&app.root_path, &tree, app.options.metric);
        app.tree = Some(Arc::new(tree));
        app.refresh_insights();
        app.select_largest();
        app
    }

    /// Scan a different root from scratch. Marks are kept; a mark outside
    /// the new root is shown as kept back, never removed.
    pub fn set_root(&mut self, root: PathBuf) {
        self.space = space_info(&root).ok();
        self.device = device_for(&root);
        self.root_path = root;
        self.screen = Screen::Explore;
        self.start_scan();
    }

    /// Go up to `above`, a directory containing the scanned root.
    ///
    /// Memoized: the tree already measured is handed to the walk and reused
    /// where it is reached, so only what is new at the wider level is read,
    /// and the current view stays on screen until the wider tree lands.
    pub fn widen_to(&mut self, above: PathBuf) {
        let Some(tree) = self.tree.clone() else {
            self.set_root(above);
            return;
        };
        if !self.root_path.starts_with(&above) || self.root_path == above {
            return;
        }
        if let Some(scan) = &self.scan {
            scan.cancel();
        }
        self.progress = ScanSnapshot::default();
        self.scan_error = None;
        self.scan_started = Some(Instant::now());
        self.scan_elapsed = None;
        self.scan_root.clone_from(&above);
        let known = Known {
            path: self.root_path.clone(),
            tree,
        };
        self.scan = Some(ScanHandle::spawn_with(
            above,
            self.options.clone(),
            Some(known),
        ));
    }

    /// `g`: the whole disk. Widens when the disk is above the scanned root,
    /// and goes to its top when it already is the root.
    pub fn go_to_disk(&mut self) {
        let Some(disk) = self.disk_root.clone() else {
            return;
        };
        if disk == self.root_path {
            self.go_to(Vec::new());
        } else {
            self.widen_to(disk);
        }
    }

    /// Recompute "worth a look" from the tree on screen.
    fn refresh_insights(&mut self) {
        self.scanned_at = now_seconds();
        self.insights = self.tree.as_deref().map_or_else(Vec::new, |tree| {
            worth_a_look(tree, self.scanned_at, INSIGHT_LIMIT)
        });
    }

    /// Ask git about `path` once, if it is a checkout. Synchronous: it is
    /// three fast invocations, cached per path, and a request can wait.
    pub fn ensure_git(&mut self, path: &Path) {
        if self.git.contains_key(path) {
            return;
        }
        let state = disktree_core::git::state(path);
        self.git.insert(path.to_path_buf(), state);
    }

    /// Select the largest entry of the current root, so the selection, the
    /// tooltip and the mark key all have something to act on from the first
    /// frame.
    fn select_largest(&mut self) {
        if self.selected.is_some() {
            return;
        }
        if let Some(index) = self.current().and_then(Node::largest_child) {
            self.selected = Some(vec![index]);
        }
    }

    // ── scanning ────────────────────────────────────────────────────────

    /// Start a fresh scan, abandoning any walk still in progress.
    pub fn start_scan(&mut self) {
        if let Some(scan) = &self.scan {
            scan.cancel();
        }
        self.progress = ScanSnapshot::default();
        self.scan_error = None;
        self.tree = None;
        self.crumbs.clear();
        self.selected = None;
        self.view = View::default();
        self.cache = None;
        self.insights.clear();
        self.git.clear();
        self.clear_filter();
        self.scan_started = Some(Instant::now());
        self.scan_elapsed = None;
        self.scan_root.clone_from(&self.root_path);
        self.scan = Some(ScanHandle::spawn(
            self.root_path.clone(),
            self.options.clone(),
        ));
    }

    /// Advance everything that runs in the background: the scan, the removal,
    /// and the free-space meter. The desktop drives these from timers; here
    /// every request pumps them, and the browser polls while anything runs.
    pub fn pump(&mut self) {
        self.poll_scan_once();
        self.poll_removal_once();
        let interval = if self.screen == Screen::Running {
            Duration::from_millis(150)
        } else {
            Duration::from_millis(1200)
        };
        if self.space_checked.elapsed() >= interval {
            self.space_checked = Instant::now();
            let fresh = space_info(&self.root_path).ok();
            if fresh != self.space {
                self.space = fresh;
            }
        }
    }

    /// Anything the browser should keep polling for.
    pub const fn busy(&self) -> bool {
        self.scan.is_some() || self.run.is_some()
    }

    /// Drain the scan channel once.
    fn poll_scan_once(&mut self) {
        let Some(scan) = &self.scan else {
            return;
        };
        self.progress = scan.progress.snapshot();
        let Some(outcome) = scan.poll() else {
            return;
        };
        match outcome {
            Ok(node) => {
                // A widening scan lands on a new root: move the view up to
                // it, with the directory it came from selected.
                let came_from = (self.scan_root != self.root_path)
                    .then(|| self.root_path.clone());
                if came_from.is_some() {
                    self.clear_filter();
                    self.root_path.clone_from(&self.scan_root);
                    self.space = space_info(&self.root_path).ok();
                    self.device = device_for(&self.root_path);
                }
                let metric = self.options.metric;
                self.marks.refresh(&self.root_path, &node, metric);
                self.tree = Some(Arc::new(node));
                self.cache = None;
                self.refresh_insights();
                self.scan_elapsed =
                    self.scan_started.map(|started| started.elapsed());
                if let Some(from) = came_from {
                    let crumbs = self.crumbs_for_path(&from);
                    self.crumbs.clear();
                    self.view = View::default();
                    self.forget_hover();
                    self.selected = crumbs.and_then(|crumbs| {
                        crumbs.first().map(|&top| vec![top])
                    });
                }
                self.keep_selection_valid();
                self.select_largest();
            }
            Err(error) => self.scan_error = Some(error.to_string()),
        }
        self.scan = None;
        self.progress.finished = true;
    }

    fn keep_selection_valid(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        if tree.resolve(&self.crumbs).is_none() {
            self.crumbs.clear();
        }
        if let Some(selected) = self.selected.clone()
            && tree.resolve(&selected).is_none()
        {
            self.selected = None;
        }
    }

    // ── navigation ──────────────────────────────────────────────────────

    pub fn tree(&self) -> Option<&Node> {
        self.tree.as_deref()
    }

    /// The node the treemap is currently rooted at.
    pub fn current(&self) -> Option<&Node> {
        self.tree
            .as_ref()
            .and_then(|tree| tree.resolve(&self.crumbs))
    }

    pub fn node_at(&self, crumbs: &[usize]) -> Option<&Node> {
        self.tree.as_ref().and_then(|tree| tree.resolve(crumbs))
    }

    pub fn path_at(&self, crumbs: &[usize]) -> Option<PathBuf> {
        let tree = self.tree.as_ref()?;
        Some(path_of(&self.root_path, tree, crumbs))
    }

    /// Path of the directory currently drawn.
    pub fn current_path(&self) -> PathBuf {
        self.path_at(&self.crumbs)
            .unwrap_or_else(|| self.root_path.clone())
    }

    /// Breadcrumb labels from the scanned root to the current directory.
    /// The trail, from `/`: the directories above the scanned root, which
    /// widen the scan, then the root and the path into the tree.
    pub fn breadcrumbs(&self) -> Vec<(String, Crumb)> {
        let mut trail: Vec<(String, Crumb)> = self
            .root_path
            .ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|path| (crumb_label(path), Crumb::Above(path.to_path_buf())))
            .collect();
        trail.push((crumb_label(&self.root_path), Crumb::Tree(Vec::new())));
        let Some(tree) = &self.tree else {
            return trail;
        };
        let mut crumbs: Vec<usize> = Vec::new();
        for &index in &self.crumbs {
            let Some(node) =
                tree.resolve(&crumbs).and_then(|node| node.child(index))
            else {
                break;
            };
            crumbs.push(index);
            trail.push((node.name.to_string(), Crumb::Tree(crumbs.clone())));
        }
        trail
    }

    /// The children of `parent`, largest first, for a sibling menu, and how
    /// many more there are beyond [`SIBLING_ROWS`].
    pub fn siblings(&self, parent: &[usize]) -> (Vec<Sibling>, usize) {
        let metric = self.options.metric;
        let Some(node) = self.node_at(parent) else {
            return (Vec::new(), 0);
        };
        let mut rows: Vec<Sibling> = node
            .children
            .iter()
            .enumerate()
            .map(|(index, child)| Sibling {
                index,
                name: child.name.to_string(),
                value: child.value(metric),
                category: child.category,
                is_dir: child.is_dir(),
            })
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.value));
        let more = rows.len().saturating_sub(SIBLING_ROWS);
        rows.truncate(SIBLING_ROWS);
        (rows, more)
    }

    /// Open the sibling menu for the crumb at `crumbs` (not the root).
    pub fn open_crumb_menu(&mut self, crumbs: &[usize], anchor: (f32, f32)) {
        let Some((&current, parent)) = crumbs.split_last() else {
            return;
        };
        let (rows, _) = self.siblings(parent);
        let highlighted = rows
            .iter()
            .position(|row| row.index == current)
            .unwrap_or(0);
        self.crumb_menu = Some(CrumbMenu {
            parent: parent.to_vec(),
            current,
            highlighted,
            anchor,
        });
    }

    /// Go to a sibling: into it when it is a directory, beside it (selected)
    /// when it is a file.
    pub fn choose_sibling(&mut self, parent: &[usize], index: usize) {
        self.crumb_menu = None;
        let mut crumbs = parent.to_vec();
        crumbs.push(index);
        if self.node_at(&crumbs).is_some_and(Node::is_dir) {
            self.go_to(crumbs);
        } else {
            self.reveal(crumbs);
        }
    }

    /// Keys while a sibling menu is open: it owns the arrows, Enter and
    /// Escape, and any other key closes it before acting.
    fn on_menu_key(&mut self, key: &str) -> bool {
        let Some(menu) = self.crumb_menu.clone() else {
            return false;
        };
        let (rows, _) = self.siblings(&menu.parent);
        let last = rows.len().saturating_sub(1);
        match key {
            "down" | "j" => {
                self.set_highlight((menu.highlighted + 1).min(last));
            }
            "up" | "k" => {
                self.set_highlight(menu.highlighted.saturating_sub(1));
            }
            "home" => self.set_highlight(0),
            "end" => self.set_highlight(last),
            "enter" | "space" | "right" | "l" => {
                if let Some(row) = rows.get(menu.highlighted) {
                    self.choose_sibling(&menu.parent, row.index);
                }
            }
            "escape" | "left" | "h" => self.crumb_menu = None,
            _ => {
                self.crumb_menu = None;
                return false;
            }
        }
        true
    }

    const fn set_highlight(&mut self, row: usize) {
        if let Some(menu) = &mut self.crumb_menu {
            menu.highlighted = row;
        }
    }

    /// Descend into the selected tile, or into the largest child of the
    /// current root when nothing is selected.
    pub fn descend(&mut self) {
        let target = match self.selected.clone() {
            // Enter what is selected, however deep: a directory opens itself,
            // a file opens the directory holding it.
            Some(selected) if selected.len() > self.crumbs.len() => {
                if self.node_at(&selected).is_some_and(Node::is_dir) {
                    selected
                } else {
                    selected[..selected.len() - 1].to_vec()
                }
            }
            // Nothing below the root is selected: the largest entry.
            _ => match self.current().and_then(Node::largest_child) {
                Some(index) => {
                    let mut crumbs = self.crumbs.clone();
                    crumbs.push(index);
                    crumbs
                }
                None => return,
            },
        };
        self.enter(target);
    }

    /// Make `target` — any directory below the current root — the root.
    fn enter(&mut self, target: Vec<usize>) {
        if target.len() <= self.crumbs.len()
            || !target.starts_with(&self.crumbs)
        {
            return;
        }
        let Some(node) = self.node_at(&target) else {
            return;
        };
        if !node.is_dir() || node.children.is_empty() {
            return;
        }
        self.selected = Some(target.clone());
        self.crumbs = target;
        self.forget_hover();
        self.cache = None;
        self.view = View::IDENTITY;
    }

    /// Where a drawn directory's contents sit: its tile below the name band.
    /// This, not the whole tile, is the region its children occupy, so it is
    /// what zooming into it has to fill.
    pub fn tile_body(&mut self, crumbs: &[usize]) -> Option<Rect> {
        let tile =
            self.layout()?.iter().find(|tile| tile.crumbs() == crumbs)?;
        Some(match tile.header {
            Some(header) => Rect::new(
                tile.rect.x,
                header.bottom(),
                tile.rect.w,
                tile.rect.bottom() - header.bottom(),
            ),
            None => tile.rect,
        })
    }

    /// The deepest drawn directory under a viewport point that has contents
    /// to show: what zooming at that point is zooming into.
    fn zoom_target(&mut self, x: f32, y: f32) -> Option<Vec<usize>> {
        let hovered = self.tile_at(x, y)?;
        let root = self.crumbs.len();
        (root + 1..=hovered.len())
            .rev()
            .map(|length| hovered[..length].to_vec())
            .find(|crumbs| {
                self.node_at(crumbs).is_some_and(|node| {
                    node.is_dir() && !node.children.is_empty()
                }) && self.tile_body(crumbs).is_some()
            })
    }

    /// Ascend to the parent directory, keeping the directory we came from
    /// selected.
    pub fn ascend(&mut self) {
        let Some(parent_crumbs) = self.parent_crumbs() else {
            return;
        };
        self.crumbs.clone_from(&parent_crumbs);
        self.forget_hover();
        self.cache = None;
        self.selected = Some(parent_crumbs);
        self.view = View::IDENTITY;
    }

    /// The crumbs of the parent of the current root, if any.
    pub fn parent_crumbs(&self) -> Option<Vec<usize>> {
        if self.crumbs.is_empty() {
            None
        } else {
            Some(self.crumbs[..self.crumbs.len() - 1].to_vec())
        }
    }

    /// Jump straight to a crumb from the breadcrumb bar.
    pub fn go_to(&mut self, crumbs: Vec<usize>) {
        self.crumb_menu = None;
        self.crumbs.clone_from(&crumbs);
        self.selected = Some(crumbs);
        self.forget_hover();
        self.view = View::IDENTITY;
        self.cache = None;
    }

    /// Show `crumbs` in its directory, selected: what a "worth a look" row
    /// or a found name does.
    pub fn reveal(&mut self, crumbs: Vec<usize>) {
        let parent = crumbs[..crumbs.len().saturating_sub(1)].to_vec();
        self.go_to(parent);
        self.selected = Some(crumbs);
        self.pointer_active = false;
    }

    /// Select the tile at `crumbs` without changing the root.
    pub fn select(&mut self, crumbs: Option<Vec<usize>>) {
        self.selected = crumbs;
    }

    /// The tile a key acts on: under the pointer if the pointer moved last,
    /// otherwise the keyboard selection.
    pub fn action_target(&self) -> Option<Vec<usize>> {
        if self.pointer_active {
            self.hovered.clone().or_else(|| self.selected.clone())
        } else {
            self.selected.clone()
        }
    }

    /// Make the pointed-at tile the selection before a key acts, so marking,
    /// opening and arrow movement all start from what the user is looking at.
    fn adopt_pointer_target(&mut self) {
        if self.pointer_active
            && let Some(hovered) = self.hovered.clone()
        {
            self.selected = Some(hovered);
        }
    }

    /// After the layout changes under a still pointer, its hover is stale
    /// until the pointer moves again.
    fn forget_hover(&mut self) {
        self.hovered = None;
        self.pointer_active = false;
    }

    /// Move the selection geometrically, falling back to the parent at an
    /// edge.
    pub fn move_selection(&mut self, direction: Direction) {
        self.pointer_active = false;
        let Some(tiles) = self.layout().map(<[Tile]>::to_vec) else {
            return;
        };
        let Some(current) = self.selected.clone() else {
            if let Some(first) = tiles.first() {
                let crumbs = first.crumbs().to_vec();
                self.select(Some(crumbs));
            }
            return;
        };
        let Some(from) = tiles
            .iter()
            .find(|tile| tile.crumbs() == current.as_slice())
            .map(|tile| self.view.project(tile.rect))
        else {
            return;
        };

        // Same-depth neighbours only: stepping into a child by arrow key
        // would make the depth of the selection impossible to predict.
        let depth = current.len();
        let mut best: Option<(f32, Vec<usize>)> = None;
        for tile in &tiles {
            let crumbs = tile.crumbs();
            if crumbs.len() != depth || crumbs == current.as_slice() {
                continue;
            }
            let rect = self.view.project(tile.rect);
            let Some(gap) = direction.gap(&from, &rect) else {
                continue;
            };
            let offset = direction.offset(&from, &rect);
            let score = offset.mul_add(2.5, gap);
            if best.as_ref().is_none_or(|(existing, _)| score < *existing) {
                best = Some((score, crumbs.to_vec()));
            }
        }

        if let Some((_, crumbs)) = best {
            self.select(Some(crumbs));
        } else if direction.is_backwards()
            && let Some(parent) = self.parent_crumbs()
        {
            self.select(Some(parent));
        }
    }

    /// Select the next sibling by rank, which is next-largest by the active
    /// metric. Scanning a directory for space is exactly this walk.
    pub fn cycle_sibling(&mut self, step: isize) {
        self.pointer_active = false;
        let siblings = self.ranked_siblings();
        if siblings.is_empty() {
            return;
        }
        let current = self.selected.as_ref().and_then(|selected| {
            siblings
                .iter()
                .position(|crumbs| crumbs.as_slice() == selected.as_slice())
        });
        let next = match current {
            Some(index) => {
                let len = siblings.len().cast_signed();
                (((index.cast_signed() + step) % len + len) % len) as usize
            }
            None if step >= 0 => 0,
            None => siblings.len() - 1,
        };
        self.select(Some(siblings[next].clone()));
    }

    fn ranked_siblings(&self) -> Vec<Vec<usize>> {
        let parent = self.selected.as_ref().map_or_else(
            || self.crumbs.clone(),
            |selected| {
                if selected.len() <= self.crumbs.len() {
                    self.crumbs.clone()
                } else {
                    selected[..selected.len() - 1].to_vec()
                }
            },
        );
        let Some(node) = self.node_at(&parent) else {
            return Vec::new();
        };
        (0..node.children.len())
            .map(|index| {
                let mut crumbs = parent.clone();
                crumbs.push(index);
                crumbs
            })
            .collect()
    }

    // ── marking ─────────────────────────────────────────────────────────

    /// Mark or unmark the current selection.
    pub fn toggle_mark_selected(&mut self) {
        let Some(crumbs) = self.selected.clone() else {
            return;
        };
        if crumbs.is_empty() {
            self.notice = Some((
                "the scanned root cannot be removed; open a directory first"
                    .into(),
                Status::Warning,
            ));
            return;
        }
        self.toggle_mark(&crumbs);
    }

    /// Mark or unmark the node at `crumbs`.
    ///
    /// A mark covers everything beneath it, since removing a directory takes
    /// its contents with it: marking a directory absorbs the marks already
    /// inside it, and a path inside a marked directory cannot be marked or
    /// kept on its own — it says which mark it goes with instead.
    pub fn toggle_mark(&mut self, crumbs: &[usize]) {
        let Some(target) = self.target_at(crumbs) else {
            return;
        };
        self.notice = None;
        if !self.marks.contains(&target.path)
            && let Some(ancestor) = self.marked_ancestor(&target.path)
        {
            self.notice = Some((
                format!(
                    "{} goes with the marked {}; unmark that to keep it",
                    display_path(&target.path, self.home.as_deref()),
                    display_path(&ancestor, self.home.as_deref())
                ),
                Status::Warning,
            ));
            return;
        }
        let path = target.path.clone();
        if self.marks.toggle(target) {
            self.space_baseline = self.space_baseline.or(self.space);
            let inside: Vec<PathBuf> = self
                .marks
                .items()
                .iter()
                .filter(|item| {
                    item.path != path && item.path.starts_with(&path)
                })
                .map(|item| item.path.clone())
                .collect();
            for inner in &inside {
                self.marks.remove(inner);
            }
            if !inside.is_empty() {
                self.notice = Some((
                    format!(
                        "{} now covers {} mark{} inside it",
                        display_path(&path, self.home.as_deref()),
                        inside.len(),
                        if inside.len() == 1 { "" } else { "s" }
                    ),
                    Status::Neutral,
                ));
            }
        }
    }

    /// The marked directory `path` is inside, if any; never `path` itself.
    pub fn marked_ancestor(&self, path: &Path) -> Option<PathBuf> {
        self.marks
            .items()
            .iter()
            .filter(|item| item.path != path && path.starts_with(&item.path))
            .map(|item| item.path.clone())
            .min_by_key(|ancestor| ancestor.as_os_str().len())
    }

    pub fn target_at(&self, crumbs: &[usize]) -> Option<Target> {
        let node = self.node_at(crumbs)?;
        let path = self.path_at(crumbs)?;
        Some(Target {
            hidden: is_hidden(&path),
            path,
            bytes: node.value(self.options.metric),
            is_dir: node.is_dir(),
        })
    }

    pub fn unmark(&mut self, path: &Path) {
        self.marks.remove(path);
    }

    pub fn clear_marks(&mut self) {
        self.marks.clear();
        self.notice = None;
    }

    /// The plan the review screen shows and the removal runs.
    pub fn plan(&self) -> Plan {
        plan(self.marks.items(), &self.root_path)
    }

    // ── layout and hit-testing ──────────────────────────────────────────

    /// Crumbs for an absolute path, if the tree still contains it.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "a marked path may have been removed from the tree already"
    )]
    pub fn crumbs_for_path(&self, path: &Path) -> Option<Vec<usize>> {
        // The path is relative to the scanned root, so the walk starts there,
        // not at the directory currently drawn.
        let relative = path.strip_prefix(&self.root_path).ok()?;
        let tree = self.tree.clone()?;
        let mut node: &Node = &tree;
        let mut crumbs = Vec::new();
        for component in relative.components() {
            let name = component.as_os_str().to_string_lossy();
            let index = node
                .children
                .iter()
                .position(|child| child.name.as_ref() == name)?;
            crumbs.push(index);
            node = node.child(index)?;
        }
        Some(crumbs)
    }

    /// Tiles for the current root and viewport size, computed once per change.
    pub fn layout(&mut self) -> Option<&[Tile]> {
        self.layout_options.header = HEADER_PX;
        self.layout_options.header_inner = HEADER_INNER_PX;
        // A filter is about the directory it was typed in: above it, it
        // would hide everything beside that directory, so it lapses.
        if self
            .matches
            .as_ref()
            .is_some_and(|matches| !self.crumbs.starts_with(&matches.base))
        {
            self.clear_filter();
        }
        let (width, height) = self.mosaic_size;
        let key = LayoutKey {
            crumbs: self.crumbs.clone(),
            width: width.round(),
            height: height.round(),
            options: self.layout_options.clone(),
            filter: self.filter_epoch,
        };
        if key.width < 1.0 || key.height < 1.0 {
            return None;
        }
        let stale = self.cache.as_ref().is_none_or(|cache| cache.key != key);
        if stale {
            let tree = self.tree.clone()?;
            let node = tree.resolve(&self.crumbs)?;
            let rect = Rect::new(0.0, 0.0, key.width, key.height);
            let filter =
                self.matches.as_deref().filter(|_| self.filter_applied);
            let tiles = layout_filtered(
                node,
                &key.crumbs,
                rect,
                self.options.metric,
                &key.options,
                filter,
            );
            self.cache = Some(LayoutCache { key, tiles });
        }
        self.cache.as_ref().map(|cache| cache.tiles.as_slice())
    }

    /// Base-space rect of the tile at `crumbs`, if it is currently drawn.
    #[cfg(test)]
    pub fn tile_rect(&mut self, crumbs: &[usize]) -> Option<Rect> {
        self.layout()?
            .iter()
            .find(|tile| tile.crumbs() == crumbs)
            .map(|tile| tile.rect)
    }

    /// The deepest tile under a viewport point.
    pub fn tile_at(&mut self, x: f32, y: f32) -> Option<Vec<usize>> {
        let (base_x, base_y) = self.view.unproject(x, y);
        let tiles = self.layout()?;
        hit(tiles, base_x, base_y).map(|tile| tile.crumbs().to_vec())
    }

    // ── view ────────────────────────────────────────────────────────────

    /// Zoom toward a point.
    ///
    /// When `descend` is set the wheel stops magnifying at the point where
    /// the directory under the pointer exactly fits the viewport, and the
    /// next notch goes inside it. That ceiling is what keeps the two gestures
    /// continuous: at the moment the level changes, the tiles inside the
    /// directory are already as large as they will be, so entering makes them
    /// grow rather than shrink.
    pub fn zoom_at(&mut self, x: f32, y: f32, factor: f32, descend: bool) {
        let (width, height) = self.mosaic_size;

        // At the bottom of the zoom, zooming out goes up a level.
        if factor < 1.0 && self.view.scale <= View::MIN_SCALE + f32::EPSILON {
            if descend {
                self.ascend();
            }
            return;
        }

        // One directory decides both how far the wheel magnifies and where it
        // then goes: the deepest one under the pointer. At the ceiling its
        // contents fill the view, so going inside continues the same motion.
        let target = if descend {
            self.zoom_target(x, y)
        } else {
            None
        };
        let body = target.as_ref().and_then(|crumbs| self.tile_body(crumbs));
        let ceiling = body
            .map_or(View::MAX_SCALE, |rect| {
                View::fit_scale(rect, width, height)
            })
            .clamp(View::MIN_SCALE, View::MAX_SCALE);

        if factor > 1.0
            && self.view.scale >= ceiling - f32::EPSILON
            && let Some(target) = target
        {
            self.enter(target);
            return;
        }

        self.view = self
            .view
            .zoomed_at(x, y, factor, ceiling)
            .clamped(width, height);
    }

    pub fn reset_view(&mut self) {
        self.view = View::default();
    }

    /// Drag the magnified view: touch's version of shift-scroll. `dx`/`dy`
    /// are screen pixels the content should move by.
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        let (width, height) = self.mosaic_size;
        self.view.origin_x -= dx / self.view.scale;
        self.view.origin_y -= dy / self.view.scale;
        self.view = self.view.clamped(width, height);
    }

    /// Change how many levels are drawn, which is the other meaning of zoom
    /// in a treemap: seeing further in without changing what is on screen.
    pub fn adjust_depth(&mut self, step: i32) {
        let depth = (self.layout_options.max_depth.cast_signed() + step)
            .clamp(1, 6) as u32;
        self.layout_options.max_depth = depth;
        self.cache = None;
    }

    pub fn toggle_metric(&mut self) {
        self.options.metric = self.options.metric.toggled();
        if let Some(tree) = &self.tree {
            let mut tree = (**tree).clone();
            disktree_core::tree::aggregate(&mut tree, self.options.metric);
            self.tree = Some(Arc::new(tree));
            let metric = self.options.metric;
            self.marks.refresh(
                &self.root_path,
                self.tree.as_ref().unwrap(),
                metric,
            );
        }
        // Children are ordered by the metric, so every crumb moved.
        self.refresh_insights();
        self.clear_filter();
        self.cache = None;
    }

    /// The Size | Files | Age choice: what areas measure, and what colour
    /// says. Age keeps areas by size, since age has no area of its own.
    pub fn set_mode(&mut self, index: usize) {
        let (metric, color) = match index {
            1 => (Metric::Files, ColorMode::Kind),
            2 => (Metric::Bytes, ColorMode::Age),
            _ => (Metric::Bytes, ColorMode::Kind),
        };
        self.color_mode = color;
        if self.options.metric != metric {
            self.toggle_metric();
        }
    }

    /// The Size | Files | Age choice currently on.
    pub const fn mode_index(&self) -> usize {
        match (self.color_mode, self.options.metric) {
            (ColorMode::Age, _) => 2,
            (ColorMode::Kind, Metric::Files) => 1,
            (ColorMode::Kind, Metric::Bytes) => 0,
        }
    }

    // ── removal ─────────────────────────────────────────────────────────

    /// The review screen's commit: move to the trash at once, or ask first
    /// for a permanent deletion, which cannot be undone.
    pub fn commit(&mut self) {
        if self.plan().is_empty() {
            return;
        }
        match self.removal_mode {
            RemovalMode::Trash => self.begin_removal(),
            RemovalMode::Permanent => {
                self.confirm_open = true;
            }
        }
    }

    /// The dialog's `Delete`.
    pub fn confirm_delete(&mut self) {
        self.confirm_open = false;
        self.begin_removal();
    }

    /// The dialog's `Cancel`, or Escape.
    pub const fn cancel_delete(&mut self) {
        self.confirm_open = false;
    }

    pub fn begin_removal(&mut self) {
        let plan = self.plan();
        if plan.is_empty() {
            self.notice = Some(("nothing is marked".into(), Status::Warning));
            return;
        }
        self.space_baseline = self.space.or(self.space_baseline);
        self.run_summary = RunSummary {
            total: plan.targets.len(),
            ..RunSummary::default()
        };
        self.run_log.clear();
        self.screen = Screen::Running;
        self.run = Some(disktree_core::removal::spawn(plan, self.removal_mode));
    }

    /// Drain the removal channel once.
    fn poll_removal_once(&mut self) {
        let Some(run) = &self.run else {
            return;
        };
        let mut finished = false;
        while let Some(event) = run.poll() {
            match event {
                RemovalEvent::Start { total } => self.run_summary.total = total,
                RemovalEvent::Item {
                    path,
                    bytes,
                    outcome,
                } => {
                    if outcome.is_ok() {
                        self.run_summary.removed += 1;
                        self.run_summary.bytes += bytes;
                    } else {
                        self.run_summary.failed += 1;
                    }
                    self.run_log.push((path, outcome));
                }
                RemovalEvent::Done {
                    removed,
                    bytes,
                    failed,
                } => {
                    self.run_summary.removed = removed;
                    self.run_summary.bytes = bytes;
                    self.run_summary.failed = failed;
                    finished = true;
                }
            }
        }
        if finished {
            self.run = None;
            self.marks.clear();
            self.screen = Screen::Done;
            // The tree on screen is now wrong; start over rather than leaving
            // numbers that include what was just removed.
            self.start_scan();
        }
    }

    // ── find and filter ─────────────────────────────────────────────────

    /// Recompute what the find text matches in the directory on screen.
    ///
    /// Synchronous, where the desktop goes off the UI thread: a request can
    /// wait the walk, and the browser already sends one request per keystroke.
    pub fn refresh_matches(&mut self) {
        let needle = self.find.clone();
        let Some(tree) =
            self.tree.clone().filter(|_| !needle.trim().is_empty())
        else {
            self.matches = None;
            self.filter_applied = false;
            self.filter_epoch += 1;
            return;
        };
        let base = self.crumbs.clone();
        self.matches = tree
            .resolve(&base)
            .and_then(|node| filter(node, &base, &needle))
            .map(Arc::new);
        self.filter_epoch += 1;
    }

    /// Enter in the find field: lay out only the matches, with the largest
    /// selected so Space marks it.
    pub fn apply_filter(&mut self) {
        self.find_open = false;
        let Some(matches) = self.matches.clone() else {
            return;
        };
        if matches.count == 0 {
            self.notice = Some((
                format!("nothing here matches {}", matches.needle),
                Status::Warning,
            ));
            return;
        }
        self.filter_applied = true;
        self.filter_epoch += 1;
        self.view = View::IDENTITY;
        self.forget_hover();
        let tree = self.tree.clone();
        self.selected = matches
            .keep
            .iter()
            .filter(|(_, keep)| **keep == Keep::Whole)
            .filter_map(|(crumbs, _)| {
                let node = tree.as_deref()?.resolve(crumbs)?;
                Some((node.bytes, crumbs))
            })
            .max_by_key(|(bytes, _)| *bytes)
            .map(|(_, crumbs)| crumbs.clone());
        self.notice = None;
    }

    /// Drop the find text and the filter with it.
    pub fn clear_filter(&mut self) {
        self.find.clear();
        self.find_open = false;
        self.matches = None;
        self.filter_applied = false;
        self.filter_epoch += 1;
    }

    // ── input ───────────────────────────────────────────────────────────

    /// Handle a key press, as the desktop's `dispatch_key` does. `key` is the
    /// GPUI-style lowercase name the browser shim sends (`enter`, `left`,
    /// `space`, `/`, …).
    pub fn on_key(&mut self, key: &str, control: bool, shift: bool) {
        // The confirmation dialog owns Enter and Escape while it is open; a
        // key that reaches here must not also act on the screen behind it.
        if self.confirm_open {
            match key {
                "enter" => self.confirm_delete(),
                "escape" => self.cancel_delete(),
                _ => {}
            }
            return;
        }

        if self.crumb_menu.is_some() && self.on_menu_key(key) {
            return;
        }

        if self.show_help {
            if matches!(key, "escape" | "?" | "/" | "q") {
                self.show_help = false;
            }
            return;
        }

        match self.screen {
            Screen::Explore => self.on_explore_key(key, control, shift),
            Screen::Review => self.on_review_key(key),
            Screen::Running => {
                if key == "escape"
                    && let Some(run) = &self.run
                {
                    run.cancel();
                    self.notice = Some((
                        "stopping after the current item".into(),
                        Status::Warning,
                    ));
                }
            }
            Screen::Done => {
                if matches!(key, "escape" | "enter") {
                    self.screen = Screen::Explore;
                }
            }
        }
    }

    fn on_explore_key(&mut self, key: &str, control: bool, shift: bool) {
        // Keys that act on "the current tile" start from the pointer when it
        // moved last.
        if matches!(
            key,
            "space"
                | "x"
                | "enter"
                | "tab"
                | "left"
                | "right"
                | "up"
                | "down"
                | "h"
                | "j"
                | "k"
                | "l"
        ) {
            self.adopt_pointer_target();
        }
        match key {
            "/" | "s" if !control => {
                self.find_open = true;
            }
            "enter" => self.descend(),
            "right" | "l" if !control => self.move_selection(Direction::Right),
            "left" | "h" if !control => self.move_selection(Direction::Left),
            "up" | "k" if !control => self.move_selection(Direction::Up),
            "down" | "j" if !control => self.move_selection(Direction::Down),
            "backspace" | "u" if !control => self.ascend(),
            // A filter is the first thing Escape takes away.
            "escape" if self.matches.is_some() => self.clear_filter(),
            "escape" => {
                if self.selected.is_some() {
                    self.selected = None;
                } else {
                    self.ascend();
                }
            }
            "space" => self.toggle_mark_selected(),
            "x" if !control => self.toggle_mark_selected(),
            "tab" => self.cycle_sibling(if shift { -1 } else { 1 }),
            "c" if !control => {
                if self.marks.is_empty() {
                    self.notice = Some((
                        "mark something first: space marks the selected tile"
                            .into(),
                        Status::Warning,
                    ));
                } else {
                    self.screen = Screen::Review;
                }
            }
            "[" => self.adjust_depth(-1),
            "]" => self.adjust_depth(1),
            "-" => {
                let (x, y) = self.half_mosaic();
                self.zoom_at(x, y, 1.0 / 1.25, false);
            }
            "=" | "+" => {
                let (x, y) = self.half_mosaic();
                self.zoom_at(x, y, 1.25, false);
            }
            "0" => self.reset_view(),
            "t" if !control => {
                let next = (self.mode_index() + 1) % 3;
                self.set_mode(next);
            }
            "r" if !control => self.start_scan(),
            "g" if !control => self.go_to_disk(),
            "i" if !control => {
                self.options.include_hidden = !self.options.include_hidden;
                self.start_scan();
            }
            "d" if !control => {
                self.options.apparent_size = !self.options.apparent_size;
                self.start_scan();
            }
            "p" if !control => {
                self.show_selection = !self.show_selection;
            }
            "?" => {
                self.show_help = true;
            }
            _ => {}
        }
    }

    fn on_review_key(&mut self, key: &str) {
        match key {
            "escape" => {
                self.screen = Screen::Explore;
            }
            "enter" => self.commit(),
            "!" => self.clear_marks(),
            "p" => {
                self.removal_mode = RemovalMode::Permanent;
            }
            "m" => {
                self.removal_mode = RemovalMode::Trash;
            }
            "?" => {
                self.show_help = true;
            }
            _ => {}
        }
    }

    fn half_mosaic(&self) -> (f32, f32) {
        (self.mosaic_size.0 / 2.0, self.mosaic_size.1 / 2.0)
    }

    /// Mouse moved over the treemap; `x`/`y` are mosaic-local pixels.
    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        let (width, height) = self.mosaic_size;
        let inside = x >= 0.0 && y >= 0.0 && x < width && y < height;
        if !inside {
            self.on_mouse_leave();
            return;
        }
        self.pointer = Some((x, y));
        self.pointer_active = true;
        self.hovered = self.tile_at(x, y);
    }

    pub fn on_mouse_leave(&mut self) {
        self.pointer = None;
        self.pointer_active = false;
        self.hovered = None;
    }

    /// Click: select, then act on a repeat click, like a file manager.
    pub fn on_mouse_down(
        &mut self,
        button: MouseButton,
        x: f32,
        y: f32,
        control: bool,
        click_count: u32,
    ) {
        let crumbs = self.tile_at(x, y);
        match button {
            MouseButton::Left if click_count >= 2 => {
                if let Some(crumbs) = crumbs {
                    self.select(Some(crumbs));
                    self.descend();
                }
            }
            MouseButton::Left if control => {
                if let Some(crumbs) = crumbs {
                    self.toggle_mark(&crumbs);
                }
            }
            MouseButton::Left => {
                let activate = crumbs.as_ref().is_some_and(|crumbs| {
                    self.selected.as_ref() == Some(crumbs)
                        && self.node_at(crumbs).is_some_and(Node::is_dir)
                });
                if activate {
                    // A second click on the selection opens it, like a file
                    // manager, without needing a double click.
                    self.descend();
                    return;
                }
                self.select(crumbs);
            }
            MouseButton::Middle => {
                if let Some(crumbs) = crumbs {
                    self.toggle_mark(&crumbs);
                }
            }
        }
    }

    /// Wheel: zoom toward the pointer, then in; shift pans instead.
    pub fn on_scroll_wheel(&mut self, x: f32, y: f32, lines: f32, shift: bool) {
        if lines.abs() < f32::EPSILON {
            return;
        }
        if shift {
            // Pan instead of zoom, for looking around a magnified view.
            self.view.origin_y =
                (self.view.origin_y - lines * 40.0 / self.view.scale).max(0.0);
            return;
        }
        let factor = if lines > 0.0 { 1.15 } else { 1.0 / 1.15 };
        self.zoom_at(x, y, factor, true);
    }
}

/// A mouse button the browser reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
}

/// A direction for geometric selection movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    const fn is_backwards(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }

    /// Distance from `from` to `rect` along the axis, if `rect` lies that way.
    fn gap(self, from: &Rect, rect: &Rect) -> Option<f32> {
        let epsilon = 0.5;
        match self {
            Self::Right => {
                let gap = rect.x - from.right();
                (gap >= -epsilon).then_some(gap.max(0.0))
            }
            Self::Left => {
                let gap = from.x - rect.right();
                (gap >= -epsilon).then_some(gap.max(0.0))
            }
            Self::Down => {
                let gap = rect.y - from.bottom();
                (gap >= -epsilon).then_some(gap.max(0.0))
            }
            Self::Up => {
                let gap = from.y - rect.bottom();
                (gap >= -epsilon).then_some(gap.max(0.0))
            }
        }
    }

    /// How far off the direction's axis `rect` sits, so the nearest tile in
    /// the direction wins rather than any tile in that half-plane.
    fn offset(self, from: &Rect, rect: &Rect) -> f32 {
        let overlap =
            |a_start: f32, a_end: f32, b_start: f32, b_end: f32| -> f32 {
                (a_end.min(b_end) - a_start.max(b_start)).max(0.0)
            };
        match self {
            Self::Left | Self::Right => {
                let shared =
                    overlap(from.y, from.bottom(), rect.y, rect.bottom());
                (from.h.min(rect.h) - shared).max(0.0)
            }
            Self::Up | Self::Down => {
                let shared =
                    overlap(from.x, from.right(), rect.x, rect.right());
                (from.w.min(rect.w) - shared).max(0.0)
            }
        }
    }
}
