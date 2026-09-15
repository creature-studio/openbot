//! spark-terminal: Terminal state management using alacritty_terminal's VT parser.
//!
//! Architecture:
//!
//! ```text
//! sandd PTY
//!     │
//!  binary stream (via transport)
//!     ▼
//! TerminalModel  ← alacritty_terminal VT parser
//!     │
//!     ▼
//! GPUI TerminalView
//! ```
//!
//! GPUI is only responsible for:
//! - Surface rendering
//! - Keyboard input forwarding
//! - Mouse / selection
//! - Scrolling
//!
//! All VT state (ANSI, cursor, colors, alternate screen, etc.)
//! is handled by alacritty_terminal.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::Term;
use parking_lot::Mutex;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Event listener (required by alacritty_terminal)
// ---------------------------------------------------------------------------

/// No-op event listener. In a real build this would forward
/// alacritty events (title change, resize, etc.) to GPUI.
#[derive(Clone)]
pub struct TerminalEventListener;

impl EventListener for TerminalEventListener {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {
        // Forward to GPUI context if needed.
    }
}

// ---------------------------------------------------------------------------
// TerminalModel
// ---------------------------------------------------------------------------

/// A single terminal session.
///
/// Wraps alacritty_terminal's Term so that incoming PTY bytes are parsed
/// through the VT state machine, and the rendered grid can be read by
/// the GPUI view.
pub struct TerminalModel {
    pub id: String,
    pub task_id: String,
    term: Arc<Mutex<Term<TerminalEventListener>>>,
}

impl TerminalModel {
    pub fn new(id: String, task_id: String, cols: u16, rows: u16) -> Self {
        let size = TermSize::new(cols as usize, rows as usize);
        let term = Term::new(
            Default::default(),
            &size,
            TerminalEventListener,
        );
        Self {
            id,
            task_id,
            term: Arc::new(Mutex::new(term)),
        }
    }

    /// Feed raw bytes from the PTY into the VT parser.
    pub fn feed(&self, data: &[u8]) {
        use alacritty_terminal::vte::ansi;
        let mut term = self.term.lock();
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut *term, data);
    }

    /// Resize the terminal.
    pub fn resize(&self, cols: u16, rows: u16) {
        let size = TermSize::new(cols as usize, rows as usize);
        let mut term = self.term.lock();
        term.resize(size);
    }

    /// Read the current grid content as plain text (for simple display).
    /// In a real GPUI view, you'd read the grid cells directly for
    /// styled rendering.
    pub fn content_plain(&self) -> String {
        let term = self.term.lock();
        let mut lines = Vec::new();
        for row in 0..term.screen_lines() {
            let mut line = String::new();
            for col in 0..term.columns() {
                line.push(term.grid()[alacritty_terminal::index::Line(row as i32)][alacritty_terminal::index::Column(col)].c);
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    /// Access the underlying Term for grid-level rendering.
    pub fn with_term<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Term<TerminalEventListener>) -> R,
    {
        let term = self.term.lock();
        f(&term)
    }
}

// ---------------------------------------------------------------------------
// Terminal manager
// ---------------------------------------------------------------------------

/// Manages multiple terminal sessions across tasks.
pub struct TerminalManager {
    terminals: dashmap::DashMap<String, Arc<TerminalModel>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            terminals: dashmap::DashMap::new(),
        }
    }

    pub fn create(&self, id: String, task_id: String, cols: u16, rows: u16) -> Arc<TerminalModel> {
        let model = Arc::new(TerminalModel::new(id.clone(), task_id, cols, rows));
        self.terminals.insert(id.clone(), model.clone());
        model
    }

    pub fn get(&self, id: &str) -> Option<Arc<TerminalModel>> {
        self.terminals.get(id).map(|r| r.value().clone())
    }

    pub fn remove(&self, id: &str) {
        self.terminals.remove(id);
    }

    pub fn feed(&self, id: &str, data: &[u8]) {
        if let Some(term) = self.terminals.get(id) {
            term.feed(data);
        }
    }
}
