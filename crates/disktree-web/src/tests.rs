//! Tests for the web front end, beside the promise they make, as in
//! `disktree-core`: the state machine against real temporary trees, and the
//! HTTP boundary over a real socket — including the run that marks a
//! directory, confirms the deletion and checks the files are gone while
//! their neighbours are untouched.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use disktree_core::scan::{ScanOptions, scan};
use disktree_core::tree::Node;

use crate::app::{Screen, View, Web};
use crate::render;

/// A tree on disk: `junk/` is the biggest, then `standalone.bin`, then
/// `keep/`. Sizes are real bytes so the scan's block accounting keeps the
/// order.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    junk: PathBuf,
    keep: PathBuf,
    standalone: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().unwrap();
    let junk = root.join("junk");
    let keep = root.join("keep");
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::create_dir_all(&keep).unwrap();
    std::fs::write(junk.join("big.bin"), vec![7_u8; 100_000]).unwrap();
    std::fs::write(junk.join("note.txt"), b"junk").unwrap();
    std::fs::write(keep.join("notes.txt"), vec![3_u8; 10_000]).unwrap();
    let standalone = root.join("standalone.bin");
    std::fs::write(&standalone, vec![9_u8; 50_000]).unwrap();
    Fixture {
        _temp: temp,
        root,
        junk,
        keep,
        standalone,
    }
}

/// The app over a real scan of the fixture, at a known mosaic size.
fn app_of(fix: &Fixture) -> Web {
    let tree = scan(&fix.root, ScanOptions::default()).expect("scan fixture");
    let mut app =
        Web::with_tree(fix.root.clone(), tree, ScanOptions::default(), 3);
    app.mosaic_size = (1200.0, 700.0);
    app
}

/// Pump until nothing runs anymore, or the fixture's small tree has had
/// more than enough time.
fn settle(app: &mut Web) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.pump();
        if !app.busy() {
            // One more pump: the removal's Done starts a rescan, and only
            // the pump after it lands leaves the app truly idle.
            app.pump();
            if !app.busy() {
                return;
            }
        }
        assert!(Instant::now() < deadline, "the app stayed busy");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_full_removal_flow_over_the_state_machine() {
    let fix = fixture();
    let mut app = app_of(&fix);
    assert_eq!(app.tree().map(|t| t.children.len()), Some(3));
    // The largest entry is selected from the first frame: junk.
    assert_eq!(app.selected, Some(vec![0]));

    app.on_key("space", false, false);
    assert_eq!(app.marks.len(), 1);
    assert_eq!(app.marks.items()[0].path, fix.junk);

    app.on_key("c", false, false);
    assert_eq!(app.screen, Screen::Review);
    let frame = render::frame(&mut app);
    assert!(frame.html.contains("Review"), "the review screen renders");
    assert!(frame.html.contains("junk"), "the marked row is named");

    // The permanent path always asks first.
    app.on_key("p", false, false);
    app.on_key("enter", false, false);
    assert!(app.confirm_open, "permanent deletion asks first");
    let frame = render::frame(&mut app);
    assert!(frame.html.contains("permanently?"), "the dialog names it");

    app.on_key("enter", false, false);
    assert!(!app.confirm_open);
    assert_eq!(app.screen, Screen::Running);
    settle(&mut app);

    assert_eq!(app.screen, Screen::Done);
    assert_eq!(app.run_summary.removed, 1);
    assert!(!fix.junk.exists(), "the marked directory is gone");
    assert!(fix.keep.exists(), "its neighbour is untouched");
    assert!(fix.standalone.exists(), "and so is the loose file");
    assert!(app.marks.is_empty(), "the run clears the marks");

    app.on_key("enter", false, false);
    assert_eq!(app.screen, Screen::Explore);
    settle(&mut app);
    let tree = app.tree().expect("the rescan landed");
    assert_eq!(tree.children.len(), 2, "the tree matches the disk again");
}

#[test]
fn trash_mode_is_offered_only_with_a_backend() {
    let fix = fixture();
    let mut app = app_of(&fix);
    app.on_key("space", false, false);
    app.on_key("c", false, false);
    let frame = render::frame(&mut app);
    if app.trash_backend.is_available() {
        assert!(frame.html.contains("Move to trash"));
    } else {
        assert!(frame.html.contains("Delete permanently"));
    }
}

#[test]
fn the_root_itself_cannot_be_marked() {
    let fix = fixture();
    let mut app = app_of(&fix);
    app.selected = Some(Vec::new());
    app.on_key("space", false, false);
    assert!(app.marks.is_empty());
    let (message, _) = app.notice.expect("the refusal says why");
    assert!(message.contains("cannot be removed"), "{message}");
}

#[test]
fn marking_a_parent_absorbs_the_marks_inside_it() {
    let fix = fixture();
    let mut app = app_of(&fix);
    // Mark big.bin inside junk, then junk itself: the inner mark is absorbed.
    app.go_to(vec![0]);
    app.select(Some(vec![0, 0]));
    app.on_key("space", false, false);
    assert_eq!(app.marks.len(), 1);
    assert_eq!(app.marks.items()[0].path, fix.junk.join("big.bin"));

    app.ascend();
    app.select(Some(vec![0]));
    app.on_key("space", false, false);
    assert_eq!(app.marks.len(), 1, "the parent absorbs the child mark");
    assert_eq!(app.marks.items()[0].path, fix.junk);

    // And inside the marked directory there is nothing to mark on its own.
    app.go_to(vec![0]);
    app.select(Some(vec![0, 0]));
    app.on_key("space", false, false);
    let (message, _) = app.notice.expect("the nesting is explained");
    assert!(message.contains("goes with the marked"), "{message}");
    assert_eq!(app.marks.len(), 1);
}

#[test]
fn the_filter_narrows_the_layout_to_matches() {
    let fix = fixture();
    let mut app = app_of(&fix);
    app.find = "big".into();
    app.refresh_matches();
    let matches = app.matches.clone().expect("a match set");
    assert_eq!(matches.count, 1);

    app.apply_filter();
    assert!(app.filter_applied);
    let tiles = app.layout().expect("tiles").to_vec();
    assert!(!tiles.is_empty());
    for tile in &tiles {
        let crumbs = tile.crumbs();
        assert!(
            crumbs.starts_with(&[0])
                || app
                    .matches
                    .as_ref()
                    .is_some_and(|m| m.keep(crumbs).is_some()),
            "an unrelated tile survived the filter: {crumbs:?}",
        );
    }
    app.clear_filter();
    assert!(!app.filter_applied);
}

#[test]
fn the_wheel_zooms_then_descends_and_zooms_back_out() {
    let fix = fixture();
    let mut app = app_of(&fix);
    // Over the middle of the biggest tile, so the gesture has a target.
    let rect = app.tile_rect(&[0]).expect("the largest tile is drawn");
    let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);

    for _ in 0..40 {
        app.on_scroll_wheel(x, y, 1.0, false);
        if !app.crumbs.is_empty() {
            break;
        }
    }
    assert!(
        !app.crumbs.is_empty(),
        "zooming far enough goes inside: {:?}",
        app.crumbs,
    );
    assert!(
        (app.view.scale - View::MIN_SCALE).abs() < f32::EPSILON,
        "the new level resets zoom",
    );

    for _ in 0..40 {
        app.on_scroll_wheel(x, y, -1.0, false);
    }
    assert!(app.crumbs.is_empty(), "zooming out comes back to the root");
}

#[test]
fn arrow_keys_move_the_selection_between_siblings() {
    let fix = fixture();
    let mut app = app_of(&fix);
    assert_eq!(app.selected, Some(vec![0]));
    app.on_key("right", false, false);
    assert_eq!(app.selected, Some(vec![1]));
    app.on_key("left", false, false);
    assert_eq!(app.selected, Some(vec![0]));
}

#[test]
fn the_sibling_menu_jumps_sideways() {
    let fix = fixture();
    let mut app = app_of(&fix);
    app.go_to(vec![0]);
    assert!(!app.crumbs.is_empty());
    app.open_crumb_menu(&[0], (100.0, 30.0));
    assert!(app.crumb_menu.is_some());
    app.on_key("down", false, false);
    app.on_key("enter", false, false);
    assert!(app.crumb_menu.is_none(), "choosing closes the menu");
}

#[test]
fn the_frame_names_every_screen_and_the_tiles() {
    let fix = fixture();
    let mut app = app_of(&fix);
    let frame = render::frame(&mut app);
    assert!(frame.html.contains("<svg"), "the mosaic is painted");
    assert!(frame.html.contains("junk"), "tile labels are emitted");
    assert!(
        frame.html.contains("id=\"hatch\""),
        "the reclaimable pattern exists"
    );
    assert!(!frame.busy, "with_tree needs no scan");
    assert!(frame.title.contains("disktree"));

    // A marked tile takes the danger fill, and everything inside it.
    app.on_key("space", false, false);
    let marked = crate::palette::css(crate::palette::marked_fill());
    let frame = render::frame(&mut app);
    assert!(
        frame.html.contains(&marked),
        "the marked fill is on the mosaic",
    );
}

#[test]
fn panning_drags_the_view_and_stays_inside() {
    let fix = fixture();
    let mut app = app_of(&fix);
    // Zoom in so there is somewhere to pan.
    app.zoom_at(600.0, 350.0, 2.0, false);
    let before = app.view;
    app.pan_by(60.0, 30.0);
    assert!(
        app.view.origin_x < before.origin_x,
        "dragging pans the view"
    );
    assert!(app.view.origin_y < before.origin_y);
    // Far past the edge: clamped, no empty margin, ever.
    for _ in 0..50 {
        app.pan_by(-4000.0, -4000.0);
    }
    assert!(app.view.origin_x >= 0.0);
    assert!(app.view.origin_y >= 0.0);
}

#[test]
fn the_touch_bar_offers_the_key_actions() {
    let fix = fixture();
    let mut app = app_of(&fix);
    let frame = render::frame(&mut app);
    assert!(frame.html.contains("id=\"touch-bar\""));
    assert!(frame.html.contains("Mark"));
    // The largest entry is selected, so Mark is live and Open is offered.
    app.on_key("space", false, false);
    let frame = render::frame(&mut app);
    assert!(frame.html.contains("Unmark"), "the bar follows the mark");
    assert!(frame.html.contains("Review · <b>1</b>"));
}

// ── HTTP ────────────────────────────────────────────────────────────────

/// One request over a real socket: the boundary the browser will use.
fn http(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let content = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, content)
}

/// A server on an ephemeral port, stopped with the test process.
fn serve_test(app: Web, token: Option<&str>) -> std::net::SocketAddr {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    // On Windows ListenAddr has no Unix variant, so this pattern is
    // irrefutable there and its lint fires; on Unix it is not. The allow is
    // for the platform where the pattern can never fail.
    #[allow(
        irrefutable_let_patterns,
        reason = "ListenAddr is a single-variant enum on Windows"
    )]
    let tiny_http::ListenAddr::IP(addr) = server.server_addr() else {
        panic!("an IP listen address");
    };
    let app = Arc::new(Mutex::new(app));
    let token = token.map(|raw| Arc::new(crate::server::Token::new(raw)));
    std::thread::spawn(move || {
        loop {
            let Ok(request) = server.recv() else {
                continue;
            };
            let app = Arc::clone(&app);
            let token = token.clone();
            std::thread::spawn(move || {
                let _ = crate::server::handle(request, &app, token.as_deref());
            });
        }
    });
    addr
}

#[test]
fn the_token_gate_holds() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), Some("s3cret"));

    let (status, _) = http(addr, "GET", "/", None);
    assert_eq!(status, 401, "no token, no page");
    let (status, _) = http(addr, "GET", "/api/frame?w=1200&h=700", None);
    assert_eq!(status, 401, "no token, no frames either");

    // The script and style tags cannot carry a token, and carry no data:
    // they are the only exemption.
    let (status, body) = http(addr, "GET", "/static/app.js", None);
    assert_eq!(status, 200, "the shim loads without a token");
    assert!(body.contains("api/input"));
    let (status, _) = http(addr, "GET", "/static/style.css", None);
    assert_eq!(status, 200);

    let (status, body) = http(addr, "GET", "/?token=s3cret", None);
    assert_eq!(status, 200);
    assert!(body.contains("disktree"), "the shell ships: {body}");
    let (status, body) =
        http(addr, "GET", "/api/frame?w=1200&h=700&token=s3cret", None);
    assert_eq!(status, 200);
    assert!(body.contains("\"html\""), "a frame is JSON: {body:.200}");
}

#[test]
fn input_over_http_marks_and_the_frame_admits_it() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);

    let mark = "{\"w\":1200,\"h\":700,\"events\":[{\"type\":\"mark\",\"crumbs\":[0]}]}";
    let (status, body) = http(addr, "POST", "/api/input", Some(mark));
    assert_eq!(status, 200);
    assert!(
        body.contains("Marked"),
        "the panel's marked section shows it: {body:.400}",
    );

    let review =
        "{\"w\":1200,\"h\":700,\"events\":[{\"type\":\"key\",\"key\":\"c\"}]}";
    let (_, body) = http(addr, "POST", "/api/input", Some(review));
    assert!(body.contains("Review"), "the key drove the screen");
    assert!(body.contains("junk"), "the row names the mark");
}

#[test]
fn removal_over_http_deletes_and_rescans() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);

    // Mark junk, choose permanent, commit, confirm — the whole destructive
    // path, through the socket it will really come over.
    let batch = concat!(
        "{\"w\":1200,\"h\":700,\"events\":[",
        "{\"type\":\"mark\",\"crumbs\":[0]},",
        "{\"type\":\"key\",\"key\":\"c\"},",
        "{\"type\":\"removal\",\"mode\":\"permanent\"},",
        "{\"type\":\"commit\"},",
        "{\"type\":\"confirm_delete\"}",
        "]}"
    );
    let (status, body) = http(addr, "POST", "/api/input", Some(batch));
    assert_eq!(status, 200);
    assert!(
        body.contains("Removing"),
        "the run starts at once: {body:.200}"
    );

    // Poll the frame until the removal and the rescan behind it settle.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, body) = http(addr, "GET", "/api/frame?w=1200&h=700", None);
        assert!(Instant::now() < deadline, "never settled: {body:.200}");
        if body.contains("\"busy\":false") && !body.contains("Removing") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!fix.junk.exists(), "the marked directory is gone");
    assert!(fix.keep.exists());
    assert!(fix.standalone.exists());
}

#[test]
fn zoom_and_pan_events_drive_the_view() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);
    let (_, before) = http(addr, "GET", "/api/frame?w=1200&h=700", None);
    let batch = concat!(
        "{\"w\":1200,\"h\":700,\"events\":[",
        "{\"type\":\"zoom\",\"x\":600,\"y\":350,\"factor\":2.0},",
        "{\"type\":\"pan\",\"dx\":40,\"dy\":10}",
        "]}"
    );
    let (status, body) = http(addr, "POST", "/api/input", Some(batch));
    assert_eq!(status, 200);
    // A camera-only gesture gets the mosaic alone, never the whole page.
    assert!(body.contains("\"mosaic\""), "a mosaic answer: {body:.120}");
    assert!(!body.contains("\"html\""), "not a full frame: {body:.120}");
    let mosaic_of =
        |html: &str| html.split_once("<svg").map(|(_, rest)| rest.to_string());
    assert_ne!(
        mosaic_of(&before),
        mosaic_of(&body),
        "zoom and pan repaint the mosaic",
    );
}

#[test]
fn pointer_traffic_is_acknowledged_without_a_frame() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);
    let batch = concat!(
        "{\"w\":1200,\"h\":700,\"events\":[",
        "{\"type\":\"move\",\"x\":100,\"y\":100},",
        "{\"type\":\"move\",\"x\":120,\"y\":110},",
        "{\"type\":\"leave\"}",
        "]}"
    );
    let (status, body) = http(addr, "POST", "/api/input", Some(batch));
    assert_eq!(status, 204, "moves are acknowledged, not rendered");
    assert!(body.is_empty());

    // But the server still tracks the pointer: space then acts on the tile
    // last under it, exactly like the desktop's pointer-first rule.
    let batch = concat!(
        "{\"w\":1200,\"h\":700,\"events\":[",
        "{\"type\":\"move\",\"x\":100,\"y\":100}",
        "]}"
    );
    http(addr, "POST", "/api/input", Some(batch));
    let (_, body) = http(
        addr,
        "POST",
        "/api/input",
        Some(
            "{\"w\":1200,\"h\":700,\"events\":[{\"type\":\"key\",\"key\":\"space\"}]}",
        ),
    );
    assert!(body.contains("Marked"), "space marked the hovered tile");
}

#[test]
fn a_gesture_that_enters_a_directory_answers_a_full_frame() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);
    // Zooming far into the biggest tile descends; the breadcrumbs and panel
    // change with the directory, so the answer is the whole page. Two events,
    // because the first clamps at the tile's fill-the-viewport ceiling and
    // the second goes inside — the desktop's "next notch goes in" rule.
    let batch = concat!(
        "{\"w\":1200,\"h\":700,\"events\":[",
        "{\"type\":\"zoom\",\"x\":600,\"y\":350,\"factor\":40.0},",
        "{\"type\":\"zoom\",\"x\":600,\"y\":350,\"factor\":40.0}",
        "]}"
    );
    let (status, body) = http(addr, "POST", "/api/input", Some(batch));
    assert_eq!(status, 200);
    assert!(
        body.contains("\"html\""),
        "descending repaints all: {body:.120}"
    );
}

#[test]
fn zooming_culls_the_tiles_off_screen() {
    let fix = fixture();
    let mut app = app_of(&fix);
    let identity = crate::mosaic::mosaic(&mut app);
    // Deep into the biggest tile: most of the tree is off-screen now.
    app.zoom_at(600.0, 350.0, 4.0, false);
    let zoomed = crate::mosaic::mosaic(&mut app);
    let count = |svg: &str| svg.matches("g-tile").count();
    assert!(
        count(&zoomed) < count(&identity),
        "culled: {} -> {}",
        count(&identity),
        count(&zoomed),
    );
    assert!(zoomed.contains("junk"), "what is on screen stays painted");
}

/// Gzip on the wire: the same JSON, a fraction of the bytes.
#[test]
fn frames_come_gzipped_when_accepted() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);

    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    let payload =
        "{\"w\":1200,\"h\":700,\"events\":[{\"type\":\"key\",\"key\":\"0\"}]}";
    let request = format!(
        "POST /api/input HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Accept-Encoding: gzip\r\nContent-Length: {}\r\n\
         Content-Type: application/json\r\n\r\n{payload}",
        payload.len(),
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header boundary");
    let headers = String::from_utf8_lossy(&raw[..split]).to_string();
    assert!(headers.contains("Content-Encoding: gzip"), "{headers}");
    let body = &raw[split + 4..];
    let mut decoded = String::new();
    flate2::read::GzDecoder::new(body)
        .read_to_string(&mut decoded)
        .expect("valid gzip");
    assert!(decoded.contains("\"html\""), "{decoded:.120}");
}

#[test]
fn a_garbage_removal_mode_never_arms_permanent_deletion() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);
    let batch = concat!(
        "{\"w\":1200,\"h\":700,\"events\":[",
        "{\"type\":\"mark\",\"crumbs\":[0]},",
        "{\"type\":\"key\",\"key\":\"c\"},",
        "{\"type\":\"removal\",\"mode\":\"delete_everything_forever\"}",
        "]}"
    );
    let (status, body) = http(addr, "POST", "/api/input", Some(batch));
    assert_eq!(status, 200);
    assert!(
        body.contains("Move to trash"),
        "an unknown mode keeps the recoverable one: {body:.200}",
    );
    assert!(!body.contains("btn danger\""), "no armed danger button");
}

/// A name that is not UTF-8: ordinary on Unix, and it must not wedge the
/// renderer — every frame while it is marked carries its path in `data-ev`.
#[cfg(unix)]
#[test]
fn non_utf8_names_survive_marking_and_unmarking() {
    use std::os::unix::ffi::OsStrExt as _;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let odd = root.join(std::ffi::OsStr::from_bytes(b"odd\xffname.bin"));
    std::fs::write(&odd, b"odd").unwrap();
    let tree = scan(&root, ScanOptions::default()).unwrap();
    let mut app = Web::with_tree(root, tree, ScanOptions::default(), 3);
    app.mosaic_size = (1200.0, 700.0);

    // Mark it (largest = only child) and render the review screen: the
    // mark row's Unmark button carries the path.
    app.on_key("space", false, false);
    assert_eq!(app.marks.len(), 1);
    app.on_key("c", false, false);
    let frame = render::frame(&mut app);
    assert!(frame.html.contains("odd"), "the row renders");

    // Round-trip the exact bytes through the codec the wire uses.
    let encoded = render::path_attr(&odd);
    assert!(encoded.contains("%FF"), "{encoded}");
    let body = format!(
        "{{\"w\":1200,\"h\":700,\"events\":[{{\"type\":\"unmark\",\"path\":\"{encoded}\"}}]}}"
    );
    let decoded: serde_json::Value = serde_json::from_str(&body).unwrap();
    let events = decoded["events"].as_array().unwrap();
    let coded = events[0]["path"].as_str().unwrap();
    let path = crate::server::decode_path(coded).expect("decodes");
    assert_eq!(path, odd, "byte-exact round trip");
}

#[test]
fn unknown_routes_are_404_and_bad_bodies_400() {
    let fix = fixture();
    let addr = serve_test(app_of(&fix), None);
    let (status, _) = http(addr, "GET", "/nope", None);
    assert_eq!(status, 404);
    let (status, _) = http(addr, "POST", "/api/input", Some("not json"));
    assert_eq!(status, 400);
}

/// A wider tree: 30 directories of 150 real files each, so renders have
/// real layout, escaping and labels to chew on.
fn big_fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().unwrap();
    for dir in 0..30 {
        let dir_path = root.join(format!("dir-{dir:02}"));
        std::fs::create_dir_all(&dir_path).unwrap();
        for file in 0..150 {
            let bytes = vec![b'x'; 4096 * (1 + (dir + file) % 7)];
            std::fs::write(dir_path.join(format!("f{file:03}.bin")), bytes)
                .unwrap();
        }
    }
    (temp, root)
}

/// Perf probe, run by hand: `cargo test -p disktree-web -- --ignored \
/// --nocapture perf_probe`. Prints the numbers the wire and the CPU pay
/// per frame; nothing asserted, this is a measuring stick not a gate.
#[test]
#[ignore = "a measuring stick, run by hand with --nocapture, not a gate"]
fn perf_probe() {
    let (_temp, root) = big_fixture();
    let started = Instant::now();
    let tree = scan(&root, ScanOptions::default()).expect("scan big fixture");
    println!("scan: {:?} for {} files", started.elapsed(), tree.files);

    let mut app = Web::with_tree(root, tree, ScanOptions::default(), 3);
    app.mosaic_size = (1440.0, 900.0);

    // Warm the layout cache, then measure steady-state renders.
    let first = render::frame(&mut app);
    println!("full frame: {} bytes", first.html.len());
    let render = Instant::now();
    for _ in 0..50 {
        let frame = render::frame(&mut app);
        std::hint::black_box(&frame);
    }
    println!("full frame render: {:?} avg", render.elapsed() / 50);

    let mosaic = crate::mosaic::mosaic(&mut app);
    println!("mosaic at identity: {} bytes", mosaic.len());
    let render = Instant::now();
    for _ in 0..50 {
        let mosaic = crate::mosaic::mosaic(&mut app);
        std::hint::black_box(&mosaic);
    }
    println!("mosaic render: {:?} avg", render.elapsed() / 50);

    // Zoomed in 3x over the middle: what a gesture frame costs there.
    app.zoom_at(720.0, 450.0, 3.0, false);
    let mosaic = crate::mosaic::mosaic(&mut app);
    println!("mosaic at 3x zoom: {} bytes", mosaic.len());

    // gzip ratios on both, at the level the server would send.
    for (what, body) in [("full frame", &first.html), ("mosaic", &mosaic)] {
        let compressed =
            miniz_oxide::deflate::compress_to_vec_zlib(body.as_bytes(), 6);
        println!(
            "{what}: {} -> {} bytes gzipped ({:.0}x)",
            body.len(),
            compressed.len(),
            body.len() as f64 / compressed.len().max(1) as f64,
        );
    }
}

/// Keep `Node` honest for the test fixtures: scans sort children largest
/// first, which every crumb above assumes.
#[test]
fn the_fixture_orders_junk_first() {
    let fix = fixture();
    let tree: Node =
        scan(&fix.root, ScanOptions::default()).expect("scan fixture");
    assert_eq!(&*tree.children[0].name, "junk");
    assert_eq!(&*tree.children[1].name, "standalone.bin");
    assert_eq!(&*tree.children[2].name, "keep");
    assert!(Path::new(&fix.junk).exists());
}
