//! The HTTP boundary: static assets, one frame endpoint, one input endpoint.
//!
//! The browser is a terminal: it asks for a frame, and it posts input events
//! back. Every decision — layout, colour, what a key does, what may be
//! deleted — is made on this side, by the same code paths the desktop runs.
//! There is no API surface beyond "render" and "act": the state machine in
//! [`crate::app`] is the application.

use std::io::Read as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use std::io::Write as _;

use crate::app::{MouseButton, Web};
use crate::render::{Frame, frame};

/// The largest input batch accepted: keystrokes and pointer moves only.
const MAX_BODY: u64 = 64 * 1024;

/// One browser event, as the shim serializes it.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    /// A key press, with GPUI-style lowercase key names.
    Key {
        key: String,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        alt: bool,
    },
    /// Pointer moved over the mosaic, in mosaic-local pixels.
    Move {
        x: f32,
        y: f32,
    },
    Leave,
    Click {
        x: f32,
        y: f32,
        button: u8,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        count: u32,
    },
    Wheel {
        x: f32,
        y: f32,
        lines: f32,
        #[serde(default)]
        shift: bool,
    },
    /// A pinch: zoom by an exact factor toward the midpoint, then in.
    Zoom {
        x: f32,
        y: f32,
        factor: f32,
    },
    /// A one-finger drag: pan the magnified view, in screen pixels.
    Pan {
        dx: f32,
        dy: f32,
    },
    /// The mosaic container's size, sent with every batch.
    Size {
        w: f32,
        h: f32,
    },
    /// The side panel's width, dragged by its edge.
    Panel {
        px: f32,
    },
    /// Breadcrumb: go to a directory in the tree.
    Goto {
        crumbs: Vec<usize>,
    },
    /// Breadcrumb above the root: widen the scan to there. The path is
    /// percent-encoded OS bytes: JSON is UTF-8, Unix filenames are not.
    Widen {
        path: String,
    },
    /// Open (or close) a crumb's sibling menu, anchored at the crumb.
    Menu {
        crumbs: Vec<usize>,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
    },
    /// A sibling menu row was chosen.
    Sibling {
        parent: Vec<usize>,
        index: usize,
    },
    /// A "worth a look" row: show the path in its directory.
    Reveal {
        crumbs: Vec<usize>,
    },
    /// The panel's mark button acts on its own target.
    Mark {
        crumbs: Vec<usize>,
    },
    /// Percent-encoded too, like [`Event::Widen`].
    Unmark {
        path: String,
    },
    /// The Size | Files | Age segmented control.
    Mode {
        index: usize,
    },
    /// The review screen's removal-mode choice.
    Removal {
        mode: String,
    },
    /// The review screen's commit button.
    Commit,
    ConfirmDelete,
    CancelDelete,
    /// The find field's text, on every keystroke.
    Find {
        text: String,
    },
    FindApply,
    FindClear,
}

/// The input batch the shim posts: the mosaic's size, the browser's
/// appearance, then what happened.
#[derive(Debug, serde::Deserialize)]
struct Input {
    #[serde(default)]
    w: f32,
    #[serde(default)]
    h: f32,
    /// `prefers-color-scheme: dark` on the client, sent every batch so the
    /// fills can never sit on the wrong side of a theme flip.
    #[serde(default)]
    dark: Option<bool>,
    #[serde(default)]
    events: Vec<Event>,
}

/// At most this many requests are in flight at once; beyond it a connection
/// is dropped unread. A browser needs a handful at a time, and an unbounded
/// thread per connection is a resource-exhaustion hole.
const MAX_INFLIGHT: usize = 128;

/// Serve the application until the process ends.
pub fn serve(app: Web, listen: SocketAddr, token: Option<&str>) -> ! {
    let server = match Server::http(listen) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("disktree-web: cannot listen on {listen}: {error}");
            std::process::exit(1);
        }
    };
    let app = Arc::new(Mutex::new(app));
    let token = token.map(|raw| Arc::new(Token::new(raw)));
    let inflight = Arc::new(AtomicUsize::new(0));
    loop {
        let Ok(request) = server.recv() else {
            continue;
        };
        if inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT {
            continue;
        }
        let app = Arc::clone(&app);
        let token = token.clone();
        let inflight = Arc::clone(&inflight);
        std::thread::spawn(move || {
            let _permit = Permit::new(inflight);
            let _ = handle(request, &app, token.as_deref());
        });
    }
}

/// Counts a handler thread while it lives.
struct Permit(Arc<AtomicUsize>);

impl Permit {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Answer one request. Never fails outward: a handler error is a 500, not a
/// dropped connection. `pub(crate)` for the socket-level tests.
pub fn handle(
    mut request: Request,
    app: &Arc<Mutex<Web>>,
    token: Option<&Token>,
) -> std::io::Result<()> {
    // Static assets are exempt: they are constant and carry no filesystem
    // data, and a <script>/<link> tag cannot send the token. The match is
    // exact — never a prefix — so no path construction can widen it.
    // Everything that can read or change the disk stays gated.
    let method = request.method().clone();
    let path = request.url().split('?').next().unwrap_or("/").to_string();
    let is_asset =
        matches!(path.as_str(), "/static/app.js" | "/static/style.css");
    if let Some(token) = token
        && !is_asset
        && !authorized(&request, token)
    {
        return request.respond(unauthorized());
    }
    match (method, path.as_str()) {
        (Method::Get, "/") => {
            // The shell is constant; revalidate so a redeploy shows.
            let response = body_response(
                &request,
                INDEX.as_bytes().to_vec(),
                "text/html; charset=utf-8",
                StatusCode(200),
                "no-cache",
            );
            request.respond(response)
        }
        (Method::Get, "/static/app.js") => {
            let response = body_response(
                &request,
                APP_JS.as_bytes().to_vec(),
                "application/javascript",
                StatusCode(200),
                "no-cache",
            );
            request.respond(response)
        }
        (Method::Get, "/static/style.css") => {
            let response = body_response(
                &request,
                STYLE_CSS.as_bytes().to_vec(),
                "text/css",
                StatusCode(200),
                "no-cache",
            );
            request.respond(response)
        }
        (Method::Get, "/api/frame") => {
            let query = request.url().split_once('?').map_or("", |(_, q)| q);
            let (w, h, dark) = params_from(query);
            respond(request, render_with(app, w, h, dark, Vec::new()))
        }
        (Method::Get, "/api/export/list") => {
            // The marked list as one path per line, for a script or a later
            // look. Content-Disposition makes the browser save it.
            let body = {
                let app = match app.lock() {
                    Ok(app) => app,
                    Err(poison) => poison.into_inner(),
                };
                disktree_core::export::delete_list(&app.plan().targets)
            };
            let mut response = body_response(
                &request,
                body.into_bytes(),
                "text/plain; charset=utf-8",
                StatusCode(200),
                "no-store",
            );
            response.add_header(
                Header::from_bytes(
                    "Content-Disposition",
                    "attachment; filename=disktree-delete-list.txt",
                )
                .unwrap(),
            );
            request.respond(response)
        }
        (Method::Get, "/api/export/prompt") => {
            // The cleanup brief for a coding agent, as text to copy.
            let body = {
                let app = match app.lock() {
                    Ok(app) => app,
                    Err(poison) => poison.into_inner(),
                };
                disktree_core::export::agent_prompt(
                    &app.plan().targets,
                    &app.root_path,
                    app.space,
                )
            };
            let response = body_response(
                &request,
                body.into_bytes(),
                "text/plain; charset=utf-8",
                StatusCode(200),
                "no-store",
            );
            request.respond(response)
        }
        (Method::Post, "/api/input") => {
            let mut body = String::new();
            let read =
                request.as_reader().take(MAX_BODY).read_to_string(&mut body);
            match (read, serde_json::from_str::<Input>(&body)) {
                (Ok(_), Ok(input)) => respond(
                    request,
                    render_with(
                        app,
                        input.w,
                        input.h,
                        input.dark,
                        input.events,
                    ),
                ),
                _ => request.respond(bad_request()),
            }
        }
        _ => request.respond(not_found()),
    }
}

/// What a request answers with, once its input is applied.
///
/// Pointer traffic dominates and changes nothing that is painted — the
/// browser draws the hover ring and tooltip itself — so it earns a bare
/// acknowledgement. View gestures repaint only the mosaic. Everything else
/// gets the full page.
pub enum Answer {
    Ack,
    Mosaic {
        mosaic: String,
        /// The zoom readout's new text ("1.4\u{00d7}", empty at 1\u{00d7}).
        zoom: String,
    },
    Frame(Frame),
}

/// Apply one batch of input and render the answer that matches it.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the state guard must live through the render, which borrows it; \
              dropping it early is not an option"
)]
fn render_with(
    app: &Arc<Mutex<Web>>,
    w: f32,
    h: f32,
    dark: Option<bool>,
    events: Vec<Event>,
) -> Answer {
    let mut app = match app.lock() {
        Ok(app) => app,
        Err(poison) => poison.into_inner(),
    };
    if let Some(dark) = dark {
        app.appearance = if dark {
            crate::palette::Appearance::Dark
        } else {
            crate::palette::Appearance::Light
        };
    }
    if w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite() {
        app.mosaic_size = (w, h);
    } else if app.mosaic_size.0 < 1.0 {
        // The shim's first call happens before the mosaic container exists,
        // so it reports nothing; give the first paint a real size rather
        // than an empty viewport. The next batch corrects it.
        app.mosaic_size = (1280.0, 800.0);
    }
    if !events.is_empty()
        && events
            .iter()
            .all(|event| matches!(event, Event::Move { .. } | Event::Leave))
    {
        for event in events {
            apply(&mut app, event);
        }
        return Answer::Ack;
    }
    let view_only = !events.is_empty()
        && events.iter().all(|event| {
            matches!(
                event,
                Event::Wheel { .. }
                    | Event::Zoom { .. }
                    | Event::Pan { .. }
                    | Event::Size { .. }
            )
        });
    let before = (app.crumbs.clone(), app.screen);
    for event in events {
        apply(&mut app, event);
    }
    // A gesture that entered or left a directory changes the whole page;
    // one that only moved the camera repaints the mosaic alone.
    if view_only && before == (app.crumbs.clone(), app.screen) {
        let zoom = if (app.view.scale - 1.0).abs() > 0.01 {
            format!("{:.1}\u{00d7}", app.view.scale)
        } else {
            String::new()
        };
        app.pump();
        return Answer::Mosaic {
            mosaic: if app.tree().is_some() {
                crate::mosaic::mosaic(&mut app)
            } else {
                String::new()
            },
            zoom,
        };
    }
    Answer::Frame(frame(&mut app))
}

/// One event against the state machine.
fn apply(app: &mut Web, event: Event) {
    match event {
        Event::Key {
            key,
            ctrl,
            shift,
            alt,
        } => {
            app.on_key(&key, ctrl, shift, alt);
        }
        Event::Move { x, y } => app.on_mouse_move(x, y),
        Event::Leave => app.on_mouse_leave(),
        Event::Click {
            x,
            y,
            button,
            ctrl,
            count,
        } => {
            // Left and Middle only, as the desktop: a right-click on the
            // mosaic does nothing (its menu is suppressed, so silently
            // treating it as a left click would surprise twice).
            let button = match button {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                _ => return,
            };
            app.on_mouse_down(button, x, y, ctrl, count);
        }
        Event::Wheel { x, y, lines, shift } => {
            if [x, y, lines].iter().all(|v| v.is_finite()) {
                app.on_scroll_wheel(x, y, lines, shift);
            }
        }
        Event::Zoom { x, y, factor } => {
            // JSON cannot say NaN, but `1e999` parses to infinity; the
            // clamps contain infinities, and nothing here may rely on that.
            if [x, y, factor].iter().all(|v| v.is_finite()) {
                app.zoom_at(x, y, factor, true);
            }
        }
        Event::Pan { dx, dy } => {
            if dx.is_finite() && dy.is_finite() {
                app.pan_by(dx, dy);
            }
        }
        Event::Size { w, h } => {
            if w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite() {
                app.mosaic_size = (w, h);
            }
        }
        Event::Panel { px } => {
            // The desktop's limits, and never so wide that the mosaic is
            // squeezed below the panel's own minimum.
            let max = (app.mosaic_size.0 + app.panel_px
                - crate::app::PANEL_MIN_PX)
                .clamp(crate::app::PANEL_MIN_PX, crate::app::PANEL_MAX_PX);
            app.panel_px = px.clamp(crate::app::PANEL_MIN_PX, max);
        }
        Event::Goto { crumbs } => app.go_to(crumbs),
        Event::Widen { path } => {
            if let Some(path) = decode_path(&path) {
                app.widen_to(path);
            }
        }
        Event::Menu { crumbs, x, y } => {
            let open = app.crumb_menu.as_ref().is_some_and(|menu| {
                menu.parent == crumbs[..crumbs.len().saturating_sub(1)]
            });
            if open {
                app.crumb_menu = None;
            } else {
                app.open_crumb_menu(&crumbs, (x, y));
            }
        }
        Event::Sibling { parent, index } => app.choose_sibling(&parent, index),
        Event::Reveal { crumbs } => app.reveal(crumbs),
        Event::Mark { crumbs } => app.toggle_mark(&crumbs),
        Event::Unmark { path } => {
            if let Some(path) = decode_path(&path) {
                app.unmark(&path);
            }
        }
        Event::Mode { index } => app.set_mode(index),
        Event::Removal { mode } => {
            // Only the exact word arms the irreversible mode; anything else,
            // including garbage, lands on the recoverable one.
            app.removal_mode = match mode.as_str() {
                "permanent" => disktree_core::removal::RemovalMode::Permanent,
                _ => disktree_core::removal::RemovalMode::Trash,
            };
        }
        Event::Commit => app.commit(),
        Event::ConfirmDelete => app.confirm_delete(),
        Event::CancelDelete => app.cancel_delete(),
        Event::Find { text } => {
            app.find = text;
            app.refresh_matches();
        }
        Event::FindApply => app.apply_filter(),
        Event::FindClear => app.clear_filter(),
    }
}

/// The shared secret, checked when one is configured: `?token=` on the URL,
/// or an `Authorization: Bearer` header.
fn authorized(request: &Request, token: &Token) -> bool {
    if let Some((_, query)) = request.url().split_once('?') {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("token=")
                && token_eq(value, &token.raw)
            {
                return true;
            }
        }
    }
    request.headers().iter().any(|header| {
        header.field.equiv("authorization")
            && token_eq(header.value.as_str(), &token.bearer)
    })
}

/// The secret, with its Bearer form precomputed once.
pub struct Token {
    raw: String,
    bearer: String,
}

impl Token {
    pub fn new(raw: &str) -> Self {
        Self {
            raw: raw.to_string(),
            bearer: format!("Bearer {raw}"),
        }
    }
}

/// Compare in time independent of where they differ: a shared secret must
/// not be readable byte by byte off the response latency.
fn token_eq(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A path back out of a `data-ev` attribute: percent-decoded OS bytes, the
/// inverse of `render::path_attr`. `None` for malformed input; the event is
/// then dropped, never guessed at.
pub fn decode_path(text: &str) -> Option<PathBuf> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = |i: usize| -> Option<u8> {
                bytes
                    .get(i)
                    .and_then(|b| char::from(*b).to_digit(16))
                    .map(|d| d as u8)
            };
            out.push((hex(at + 1)? << 4) | hex(at + 2)?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(&out)))
    }
    // The app is a Linux tool head to toe; on anything else, only UTF-8
    // paths round-trip (no unchecked OS-string construction here).
    #[cfg(not(unix))]
    {
        String::from_utf8(out).ok().map(PathBuf::from)
    }
}

/// `w`/`h` out of a query string.
/// The frame query's `w`/`h`, and its `dark` when present.
fn params_from(query: &str) -> (f32, f32, Option<bool>) {
    let mut size = (0.0, 0.0);
    let mut dark = None;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("w=") {
            size.0 = value.parse().unwrap_or(0.0);
        } else if let Some(value) = pair.strip_prefix("h=") {
            size.1 = value.parse().unwrap_or(0.0);
        } else if let Some(value) = pair.strip_prefix("dark=") {
            dark = Some(value == "1");
        }
    }
    (size.0, size.1, dark)
}

const INDEX: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLE_CSS: &str = include_str!("../assets/style.css");

fn base_headers(cache: &'static str) -> Vec<Header> {
    // The UI is one page of self-contained assets; nothing from elsewhere
    // may load, nothing here may be framed or sniffed into another type,
    // and no referrer — the URL can carry the token — leaves the page.
    // img-src data: is the inline SVG favicon in index.html; nothing else
    // loads images at all.
    let security = "default-src 'self'; style-src 'self' 'unsafe-inline'; \
                    script-src 'self'; img-src 'self' data:; \
                    frame-ancestors 'none'";
    vec![
        Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap(),
        Header::from_bytes("Content-Security-Policy", security).unwrap(),
        Header::from_bytes("Referrer-Policy", "no-referrer").unwrap(),
        Header::from_bytes("Cache-Control", cache).unwrap(),
    ]
}

fn with_headers(
    mut response: Response<std::io::Cursor<Vec<u8>>>,
    content_type: &str,
    cache: &'static str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    for header in base_headers(cache) {
        response.add_header(header);
    }
    response
        .add_header(Header::from_bytes("Content-Type", content_type).unwrap());
    response
}

fn respond(request: Request, answer: Answer) -> std::io::Result<()> {
    match answer {
        Answer::Ack => request.respond(with_headers(
            Response::from_string(String::new())
                .with_status_code(StatusCode(204)),
            "text/plain",
            "no-store",
        )),
        Answer::Mosaic { mosaic, zoom } => {
            let body = serde_json::json!({ "mosaic": mosaic, "zoom": zoom })
                .to_string();
            let response = body_response(
                &request,
                body.into_bytes(),
                "application/json",
                StatusCode(200),
                "no-store",
            );
            request.respond(response)
        }
        Answer::Frame(frame) => {
            let body = serde_json::to_string(&frame).unwrap_or_else(|_| {
                "{\"html\":\"\",\"title\":\"disktree\",\"busy\":false,\"find_open\":false,\"find\":\"\"}".into()
            });
            let response = body_response(
                &request,
                body.into_bytes(),
                "application/json",
                StatusCode(200),
                "no-store",
            );
            request.respond(response)
        }
    }
}

/// The frames are the same few hundred elements over and over, which gzip
/// squeezes about 24-to-1: the difference between a gesture the wire notices
/// and one it doesn't. Skipped for tiny bodies and identity-only clients.
fn body_response(
    request: &Request,
    body: Vec<u8>,
    content_type: &str,
    status: StatusCode,
    cache: &'static str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let accepts_gzip = request.headers().iter().any(|header| {
        header.field.equiv("accept-encoding")
            && header.value.as_str().contains("gzip")
    });
    let (body, gzipped) = if accepts_gzip && body.len() > 1024 {
        let mut encoder = flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::new(6),
        );
        match encoder.write_all(&body).and_then(|()| encoder.finish()) {
            Ok(compressed) => (compressed, true),
            Err(_) => (body, false),
        }
    } else {
        (body, false)
    };
    let mut response = with_headers(
        Response::from_data(body).with_status_code(status),
        content_type,
        cache,
    );
    if gzipped {
        response.add_header(
            Header::from_bytes("Content-Encoding", "gzip").unwrap(),
        );
        response
            .add_header(Header::from_bytes("Vary", "Accept-Encoding").unwrap());
    }
    response
}

fn unauthorized() -> Response<std::io::Cursor<Vec<u8>>> {
    with_headers(
        Response::from_string(
            "disktree-web: this server was started with --token; \
             open it with ?token=… on the URL",
        )
        .with_status_code(StatusCode(401)),
        "text/plain",
        "no-store",
    )
}

fn bad_request() -> Response<std::io::Cursor<Vec<u8>>> {
    with_headers(
        Response::from_string("disktree-web: unreadable input")
            .with_status_code(StatusCode(400)),
        "text/plain",
        "no-store",
    )
}

fn not_found() -> Response<std::io::Cursor<Vec<u8>>> {
    with_headers(
        Response::from_string("disktree-web: not found")
            .with_status_code(StatusCode(404)),
        "text/plain",
        "no-store",
    )
}
