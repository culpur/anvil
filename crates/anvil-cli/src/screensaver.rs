//! Anvil TUI Furnace Screensaver — screen burn-in protection with branded ASCII animation.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::unused_self,
    clippy::empty_line_after_doc_comments
)]

/// Anvil TUI Furnace Screensaver — screen burn-in protection with branded ASCII animation.
///
/// Activation:
///   - `/sleep` command (explicit)
///   - 15 minutes of keyboard idle (automatic)
///
/// Animation phases:
///   1. Gathering  (3 s) — visible text slides / falls toward bottom center
///   2. Crucible   (3 s) — characters collect into a swirling pot, sparks rise
///   3. Furnace    (2 s) — crucible slides into large ASCII furnace, door closes
///   4. Melting    (loop) — furnace glows and pulses; embers drift; logo appears
///   5. Resuming   (0.5 s) — quick fade-in back to the normal TUI
///
/// Any keypress during the screensaver calls `resume()` which transitions to
/// `Resuming` and returns control to the TUI on the next tick.

use std::time::{Duration, Instant};

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

// ─── Timing constants ─────────────────────────────────────────────────────────

const GATHERING_SECS: f64 = 3.0;
const CRUCIBLE_SECS:  f64 = 3.0;
const FURNACE_SECS:   f64 = 2.0;
#[allow(dead_code)]
const RESUME_SECS:    f64 = 0.5;
/// How long the user must be idle before the screensaver activates automatically.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Target frame rate for screensaver animation.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(100);

// ─── Colours ──────────────────────────────────────────────────────────────────

const COL_IRON:      Color = Color::Rgb(139, 69,  19);  // brown/iron frame
const COL_IRON_DIM:  Color = Color::Rgb( 90, 45,  12);  // dim iron
const COL_RED_ORANGE:Color = Color::Rgb(255, 69,   0);  // molten — hot
const COL_ORANGE:    Color = Color::Rgb(255,165,   0);  // molten — mid
const COL_GOLD:      Color = Color::Rgb(255,215,   0);  // molten — cool
const COL_EMBER:     Color = Color::Rgb(255,200,  50);  // floating embers
const COL_HEAT_WAVE: Color = Color::Rgb(255,100,  50);  // heat shimmer
#[allow(dead_code)]
const COL_BLUE:      Color = Color::Rgb(  0,133, 255);  // Culpur/Anvil brand
#[allow(dead_code)]
const COL_DIM_BLUE:  Color = Color::Rgb(  0, 60, 140);  // dim brand
#[allow(dead_code)]
const COL_FALLING:   Color = Color::Rgb(160,160, 180);  // falling characters
#[allow(dead_code)]
const COL_SPARK:     Color = Color::Rgb(255,220,  80);  // sparks in crucible
const COL_STATUS:    Color = Color::DarkGray;

// ─── LCG pseudo-random (no external crate needed) ────────────────────────────

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0xDEAD_BEEF_CAFE_BABE)
    }

    fn next(&mut self) -> u64 {
        // Knuth multiplicative LCG
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    /// Random usize in [0, n)
    fn range(&mut self, n: usize) -> usize {
        if n == 0 { return 0; }
        (self.next() as usize) % n
    }

    /// Random f64 in [0, 1)
    fn frac(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ─── Particle systems ─────────────────────────────────────────────────────────

/// A single falling text character during the Gathering phase.
#[derive(Clone)]
struct FallingChar {
    ch:    char,
    /// Fractional column position (0.0 = left edge of terminal).
    x:     f64,
    /// Fractional row position (0.0 = top).
    y:     f64,
    /// Target x (bottom-centre of screen).
    tx:    f64,
    /// Target y.
    ty:    f64,
    speed: f64,
    alpha: f64, // 1.0 = opaque, fades as it gets close to target
}

/// A spark/ember floating upward.
#[derive(Clone)]
struct Ember {
    ch:    char,
    x:     f64,
    y:     f64,
    vx:    f64,
    vy:    f64, // negative = upward
    life:  f64, // 1.0 = fresh, 0.0 = dead
    decay: f64,
}

// ─── Phase ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FurnacePhase {
    Gathering,
    Crucible,
    Furnace,
    Melting,
    Resuming,
}

// ─── FurnaceScreensaver ───────────────────────────────────────────────────────

pub struct FurnaceScreensaver {
    /// When was the screensaver activated.
    activated_at:   Instant,
    /// Monotonic frame counter (increments each tick regardless of phase).
    pub frame:      usize,
    /// Current animation phase.
    pub phase:      FurnacePhase,
    /// Text lines captured from the TUI at the moment of activation.
    #[allow(dead_code)]
    captured_lines: Vec<String>,
    /// Falling character particles (Gathering phase).
    falling:        Vec<FallingChar>,
    /// Ember/spark particles (Crucible + Melting phases).
    embers:         Vec<Ember>,
    /// Molten metal animation index (cycles through frames).
    melt_frame:     usize,
    /// Last tick timestamp for delta-time calculations.
    last_tick:      Instant,
    /// Seeded pseudo-random state.
    rng:            Lcg,
    /// How far the crucible has slid into the furnace door (0.0–1.0).
    crucible_slide: f64,
    /// Furnace door open amount for the Resuming phase (0.0–1.0).
    door_open:      f64,
    /// Fade-in alpha for the Resuming phase.
    resume_alpha:   f64,
    /// True after `resume()` was called.
    resuming:       bool,
}

impl FurnaceScreensaver {
    /// Create a new screensaver, capturing the current on-screen text.
    pub fn new(captured_lines: Vec<String>) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(12345);

        let mut rng = Lcg::new(seed);

        // Seed the falling characters from the captured lines.
        let mut falling = Vec::new();
        for (row, line) in captured_lines.iter().enumerate() {
            for (col, ch) in line.char_indices() {
                if ch.is_whitespace() { continue; }
                // Don't add every character — thin it out for visual clarity.
                if rng.range(3) != 0 { continue; }
                falling.push(FallingChar {
                    ch,
                    x:     col as f64,
                    y:     row as f64,
                    tx:    0.0, // set at render time relative to terminal width
                    ty:    0.0, // set at render time relative to terminal height
                    speed: 0.3 + rng.frac() * 0.7,
                    alpha: 1.0,
                });
            }
        }

        // If we had no captured text, seed some random characters.
        if falling.is_empty() {
            let chars = ['A','n','v','i','l','·','◆','─','│','╔','╗','╚','╝'];
            for _ in 0..40 {
                let ch = chars[rng.range(chars.len())];
                falling.push(FallingChar {
                    ch,
                    x:     rng.frac() * 80.0,
                    y:     rng.frac() * 24.0,
                    tx:    0.0,
                    ty:    0.0,
                    speed: 0.3 + rng.frac() * 0.7,
                    alpha: 1.0,
                });
            }
        }

        Self {
            activated_at:   Instant::now(),
            frame:          0,
            phase:          FurnacePhase::Gathering,
            captured_lines,
            falling,
            embers:         Vec::new(),
            melt_frame:     0,
            last_tick:      Instant::now(),
            rng,
            crucible_slide: 0.0,
            door_open:      0.0,
            resume_alpha:   0.0,
            resuming:       false,
        }
    }

    /// Advance the animation by one tick.
    /// Returns `true` if the frame changed and a redraw is needed.
    /// Returns `false` when the Resuming phase is complete (caller should remove the screensaver).
    pub fn tick(&mut self, terminal_width: u16, terminal_height: u16) -> bool {
        let dt = self.last_tick.elapsed().as_secs_f64();
        self.last_tick = Instant::now();
        self.frame = self.frame.wrapping_add(1);
        self.melt_frame = self.melt_frame.wrapping_add(1);

        let elapsed = self.activated_at.elapsed().as_secs_f64();
        let cx = f64::from(terminal_width) / 2.0;
        let cy = f64::from(terminal_height) * 0.75;

        // Update target positions for falling chars.
        for fc in &mut self.falling {
            fc.tx = cx;
            fc.ty = cy;
        }

        match self.phase {
            FurnacePhase::Gathering => {
                // Move each character toward the crucible target.
                for fc in &mut self.falling {
                    let dx = fc.tx - fc.x;
                    let dy = fc.ty - fc.y;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist > 1.0 {
                        fc.x += dx * fc.speed * dt * 2.0;
                        fc.y += dy * fc.speed * dt * 2.0;
                        fc.alpha = (fc.alpha - dt * 0.1).max(0.3);
                    }
                }
                if elapsed >= GATHERING_SECS {
                    self.phase = FurnacePhase::Crucible;
                    self.spawn_sparks(terminal_width, terminal_height, 20);
                }
            }
            FurnacePhase::Crucible => {
                let phase_t = elapsed - GATHERING_SECS;
                // Swirl remaining characters in a tightening spiral.
                let angle_base = phase_t * 3.0;
                for (i, fc) in self.falling.iter_mut().enumerate() {
                    let angle = angle_base + i as f64 * 0.3;
                    let radius = ((cy - fc.y).abs() + 1.0) * (1.0 - phase_t / CRUCIBLE_SECS * 0.9);
                    let tx = cx + radius.min(8.0) * angle.cos();
                    let ty = cy + radius.min(4.0) * angle.sin();
                    fc.x += (tx - fc.x) * dt * 4.0;
                    fc.y += (ty - fc.y) * dt * 4.0;
                    fc.alpha = (1.0 - phase_t / CRUCIBLE_SECS * 0.8).max(0.1);
                }
                // Tick embers.
                self.tick_embers(dt);
                // Spawn more sparks periodically.
                if self.frame.is_multiple_of(5) {
                    self.spawn_sparks(terminal_width, terminal_height, 3);
                }
                if phase_t >= CRUCIBLE_SECS {
                    self.phase = FurnacePhase::Furnace;
                }
            }
            FurnacePhase::Furnace => {
                let phase_t = elapsed - GATHERING_SECS - CRUCIBLE_SECS;
                // Slide the crucible representation into the furnace door.
                self.crucible_slide = (phase_t / FURNACE_SECS).min(1.0);
                self.tick_embers(dt);
                if phase_t >= FURNACE_SECS {
                    self.phase = FurnacePhase::Melting;
                    // Prime a generous set of embers.
                    self.spawn_sparks(terminal_width, terminal_height, 30);
                }
            }
            FurnacePhase::Melting => {
                self.tick_embers(dt);
                // Keep a steady trickle of new embers.
                if self.frame.is_multiple_of(8) && self.embers.len() < 40 {
                    self.spawn_sparks(terminal_width, terminal_height, 2);
                }
                // If resume was requested, transition.
                if self.resuming {
                    self.phase = FurnacePhase::Resuming;
                    self.door_open = 0.0;
                }
            }
            FurnacePhase::Resuming => {
                self.door_open   = (self.door_open   + dt * 2.0).min(1.0);
                self.resume_alpha = (self.resume_alpha + dt * 3.0).min(1.0);
                if self.resume_alpha >= 1.0 {
                    // Done — signal caller to remove the screensaver.
                    return false;
                }
                self.tick_embers(dt);
            }
        }
        true
    }

    /// Signal that the user pressed a key — begin the resume animation.
    pub fn resume(&mut self) {
        self.resuming = true;
        // If we haven't reached the melting loop yet, jump directly there
        // so the resume transition always plays from a defined state.
        if !matches!(self.phase, FurnacePhase::Melting | FurnacePhase::Resuming) {
            self.phase = FurnacePhase::Melting;
            self.resuming = true;
        }
    }

    /// True while the screensaver is still running (not yet fully resumed).
    pub fn is_active(&self) -> bool {
        self.phase != FurnacePhase::Resuming || self.resume_alpha < 1.0
    }

    // ─── Particle helpers ─────────────────────────────────────────────────────

    fn spawn_sparks(&mut self, w: u16, h: u16, count: usize) {
        let cx = f64::from(w) / 2.0;
        let cy = f64::from(h) * 0.65;
        let spark_chars = ['*', '·', '˙', '°', '✦', '✧', '⁺'];
        for _ in 0..count {
            let ch = spark_chars[self.rng.range(spark_chars.len())];
            let spread = 6.0;
            self.embers.push(Ember {
                ch,
                x:     cx + (self.rng.frac() - 0.5) * spread,
                y:     cy,
                vx:    (self.rng.frac() - 0.5) * 1.5,
                vy:    -(0.8 + self.rng.frac() * 2.0), // upward
                life:  1.0,
                decay: 0.3 + self.rng.frac() * 0.5,
            });
        }
        // Cap total embers.
        if self.embers.len() > 60 {
            let drain = self.embers.len() - 60;
            self.embers.drain(0..drain);
        }
    }

    fn tick_embers(&mut self, dt: f64) {
        for e in &mut self.embers {
            e.x    += e.vx * dt * 8.0;
            e.y    += e.vy * dt * 8.0;
            e.vx   += (self.rng.next() as f64 / u64::MAX as f64 - 0.5) * dt * 0.5;
            e.life  = (e.life - e.decay * dt).max(0.0);
        }
        self.embers.retain(|e| e.life > 0.05);
    }

    // ─── Render ───────────────────────────────────────────────────────────────

    /// Draw the screensaver onto the given `Frame`.  Call this instead of the
    /// normal TUI draw whenever the screensaver is active.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Black out the whole terminal first.
        frame.render_widget(
            Paragraph::new("").style(Style::default().bg(Color::Black)),
            area,
        );

        match self.phase {
            FurnacePhase::Gathering => self.render_gathering(frame, area),
            FurnacePhase::Crucible  => self.render_crucible(frame, area),
            FurnacePhase::Furnace   => self.render_furnace_entry(frame, area),
            FurnacePhase::Melting   => self.render_melting(frame, area),
            FurnacePhase::Resuming  => self.render_resuming(frame, area),
        }
    }

    // ── Phase 1: Gathering ────────────────────────────────────────────────────

    fn render_gathering(&self, frame: &mut Frame, area: Rect) {
        let w = area.width  as usize;
        let h = area.height as usize;

        // Build a sparse grid of falling characters.
        let mut grid: Vec<Vec<Option<(char, Style)>>> = vec![vec![None; w]; h];

        for fc in &self.falling {
            let col = fc.x.round() as isize;
            let row = fc.y.round() as isize;
            if col >= 0 && col < w as isize && row >= 0 && row < h as isize {
                let brightness = (fc.alpha * 180.0) as u8;
                let style = Style::default()
                    .fg(Color::Rgb(brightness, brightness.saturating_add(20), brightness))
                    .add_modifier(Modifier::DIM);
                grid[row as usize][col as usize] = Some((fc.ch, style));
            }
        }

        // Render each row as a ratatui `Line`.
        let lines: Vec<Line<'static>> = grid
            .into_iter()
            .map(|row| {
                let spans: Vec<Span<'static>> = row
                    .into_iter()
                    .map(|cell| match cell {
                        Some((ch, style)) => Span::styled(ch.to_string(), style),
                        None              => Span::raw(" "),
                    })
                    .collect();
                Line::from(spans)
            })
            .collect();

        frame.render_widget(
            Paragraph::new(Text::from(lines)).style(Style::default().bg(Color::Black)),
            area,
        );

        self.render_blacksmith_hint(frame, area);
    }

    /// Small hint text near the bottom during gathering.
    fn render_blacksmith_hint(&self, frame: &mut Frame, area: Rect) {
        let msg = "  (⊙_☉) gathering materials…";
        let x   = area.x;
        let y   = area.y + area.height.saturating_sub(2);
        let hint_area = Rect { x, y, width: area.width, height: 1 };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(COL_STATUS))),
            hint_area,
        );
    }

    // ── Phase 2: Crucible ─────────────────────────────────────────────────────

    fn render_crucible(&self, frame: &mut Frame, area: Rect) {
        // Render the falling chars (still moving/fading).
        self.render_gathering(frame, area);

        // Draw the crucible at the bottom centre.
        let w  = area.width as usize;
        let cx = w / 2;
        let cy = area.height.saturating_sub(8) as usize;

        let crucible_art = [
            "    ╔════════╗    ",
            "   ╔╝ ~*~ ~* ╚╗   ",
            "   ║ *~~ ~~*~ ║   ",
            "   ╚═════════╝   ",
            "     ╚═══════╝     ",
            "       ╚═══╝       ",
        ];

        for (i, row) in crucible_art.iter().enumerate() {
            let y = (cy + i) as u16;
            if y >= area.y + area.height { break; }
            let x_off = cx.saturating_sub(row.len() / 2) as u16;
            let art_area = Rect {
                x: area.x + x_off,
                y,
                width:  row.len() as u16,
                height: 1,
            };
            // Alternate molten colours.
            let col = if (self.melt_frame / 3 + i).is_multiple_of(3) {
                COL_RED_ORANGE
            } else if (self.melt_frame / 3 + i) % 3 == 1 {
                COL_ORANGE
            } else {
                COL_GOLD
            };
            frame.render_widget(
                Paragraph::new(Span::styled(*row, Style::default().fg(col))),
                art_area,
            );
        }

        // Draw embers.
        self.render_embers(frame, area);
    }

    // ── Phase 3: Furnace entry ────────────────────────────────────────────────

    fn render_furnace_entry(&self, frame: &mut Frame, area: Rect) {
        // Draw the furnace body (same as melting but door is partially open).
        self.render_furnace_body(frame, area, false);

        // Overlay the sliding crucible.
        let slide = self.crucible_slide;
        let w     = area.width  as usize;
        let h     = area.height as usize;
        let cx    = w / 2;
        // The crucible starts at the bottom and rises into the furnace door.
        let start_y = h.saturating_sub(4);
        let end_y   = h / 2 + 3;
        let y = start_y - ((start_y - end_y) as f64 * slide).round() as usize;

        let crucible = "   ╔════════╗   ";
        let x_off = cx.saturating_sub(crucible.len() / 2) as u16;
        let crucible_y = (area.y as usize + y) as u16;
        if crucible_y < area.y + area.height {
            frame.render_widget(
                Paragraph::new(Span::styled(crucible, Style::default().fg(COL_IRON))),
                Rect { x: area.x + x_off, y: crucible_y, width: crucible.len() as u16, height: 1 },
            );
        }

        self.render_embers(frame, area);
    }

    // ── Phase 4: Melting (main idle loop) ─────────────────────────────────────

    fn render_melting(&self, frame: &mut Frame, area: Rect) {
        self.render_furnace_body(frame, area, true);
        self.render_embers(frame, area);
        self.render_status_line(frame, area);
    }

    // ── Phase 5: Resuming ─────────────────────────────────────────────────────

    fn render_resuming(&self, frame: &mut Frame, area: Rect) {
        self.render_furnace_body(frame, area, true);
        self.render_embers(frame, area);
        // Fade in a "resuming" overlay.
        let alpha = self.resume_alpha;
        if alpha > 0.05 {
            let br = (alpha * 220.0) as u8;
            let w  = area.width.min(40);
            let h  = 3u16;
            let x  = area.x + (area.width.saturating_sub(w)) / 2;
            let y  = area.y + (area.height.saturating_sub(h)) / 2;
            let popup_area = Rect { x, y, width: w, height: h };
            frame.render_widget(Clear, popup_area);
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        "  Resuming session…  ",
                        Style::default()
                            .fg(Color::Rgb(br, br, br))
                            .bg(Color::Rgb(20, 20, 30))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
            ];
            frame.render_widget(
                Paragraph::new(Text::from(lines))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Rgb(br / 3, br / 3, br / 2))),
                    )
                    .alignment(Alignment::Center)
                    .style(Style::default().bg(Color::Rgb(20, 20, 30))),
                popup_area,
            );
        }
    }

    // ─── Shared furnace body ──────────────────────────────────────────────────

    fn render_furnace_body(&self, frame: &mut Frame, area: Rect, show_logo: bool) {
        let w  = area.width  as usize;
        let h  = area.height as usize;
        let cx = w / 2;

        // ── Logo (appears above the furnace during Melting) ───────────────────
        if show_logo {
            self.render_logo(frame, area, cx);
        }

        // ── Heat waves above the furnace ──────────────────────────────────────
        let furnace_top_y = h / 2 + 1;
        let wave_chars = ['~', '~', ' ', '~', '~', ' ', '~'];
        for wave_row in 0..3usize {
            let y = furnace_top_y.saturating_sub(3 - wave_row);
            if y == 0 { continue; }
            let phase_offset = (self.melt_frame / 2 + wave_row * 3) % wave_chars.len();
            let wave_width   = 22 + wave_row * 2;
            let wave_x       = cx.saturating_sub(wave_width / 2);
            let mut wave_str = String::new();
            for i in 0..wave_width {
                let idx = (i + phase_offset) % wave_chars.len();
                wave_str.push(wave_chars[idx]);
            }
            let wy = (area.y as usize + y) as u16;
            if wy < area.y + area.height {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        wave_str,
                        Style::default().fg(COL_HEAT_WAVE).add_modifier(Modifier::DIM),
                    )),
                    Rect {
                        x:      area.x + wave_x as u16,
                        y:      wy,
                        width:  (wave_width + 2) as u16,
                        height: 1,
                    },
                );
            }
        }

        // ── Furnace outer frame ───────────────────────────────────────────────
        let furnace_width:  u16 = 26;
        let furnace_height: u16 = 10;
        let fx = area.x + (area.width.saturating_sub(furnace_width)) / 2;
        let fy = area.y + (h as u16 / 2);
        let furnace_area = Rect {
            x:      fx,
            y:      fy,
            width:  furnace_width,
            height: furnace_height,
        };

        // Frame border (iron colour).
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COL_IRON).add_modifier(Modifier::BOLD))
                .style(Style::default().bg(Color::Rgb(15, 8, 3))),
            furnace_area,
        );

        // ── Inner molten window ───────────────────────────────────────────────
        let inner_width:  u16 = furnace_width.saturating_sub(4);
        let inner_height: u16 = furnace_height.saturating_sub(4);
        let ix = fx + 2;
        let iy = fy + 2;
        if inner_height >= 1 {
            let inner_area = Rect {
                x:      ix,
                y:      iy,
                width:  inner_width,
                height: inner_height,
            };
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(COL_IRON_DIM))
                    .style(Style::default().bg(Color::Rgb(30, 12, 5))),
                inner_area,
            );

            // Animate the molten metal rows.
            let melt_patterns = [
                "▓▓▒▒░░▓▓▒▒░░▓▓▒▒",
                "▒▓█▓▓▒▒░▓█▓▓▒▒░▒",
                "░▒▓█▓▒░░▒▓█▓▒░░▒",
                "▒▓█▓▒░▓▒░▓█▓▒░▓▒",
                "▓▓▒▒░░▓▓▒▒░░▓▓▒▒",
            ];
            let col_cycle = [COL_RED_ORANGE, COL_ORANGE, COL_GOLD, COL_ORANGE, COL_RED_ORANGE];

            for row in 0..inner_height.saturating_sub(2) {
                let pat_idx  = (self.melt_frame / 2 + row as usize) % melt_patterns.len();
                let col_idx  = (self.melt_frame / 4 + row as usize) % col_cycle.len();
                let pattern  = melt_patterns[pat_idx];
                // Truncate or pad to fit inner_width - 2.
                let display_w = inner_width.saturating_sub(2) as usize;
                let text: String = pattern.chars().cycle().take(display_w).collect();
                let melt_y = iy + 1 + row;
                if melt_y < fy + furnace_height.saturating_sub(1) {
                    frame.render_widget(
                        Paragraph::new(Span::styled(text, Style::default().fg(col_cycle[col_idx]))),
                        Rect { x: ix + 1, y: melt_y, width: display_w as u16, height: 1 },
                    );
                }
            }
        }

        // ── Furnace base (tuyeres / blower) ───────────────────────────────────
        let base_y = fy + furnace_height;
        if base_y < area.y + area.height {
            let base = "▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓";
            frame.render_widget(
                Paragraph::new(Span::styled(
                    base,
                    Style::default().fg(COL_IRON).bg(Color::Rgb(20, 10, 3)),
                )),
                Rect {
                    x:      fx,
                    y:      base_y,
                    width:  furnace_width,
                    height: 1,
                },
            );
        }
        // Bellows connector.
        let connector = "       ◄══════════════►       ";
        let conn_y = fy + furnace_height.saturating_sub(2);
        if conn_y < area.y + area.height {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    connector,
                    Style::default().fg(COL_IRON_DIM),
                )),
                Rect {
                    x:      fx,
                    y:      conn_y,
                    width:  furnace_width,
                    height: 1,
                },
            );
        }
    }

    // ─── Logo ─────────────────────────────────────────────────────────────────

    fn render_logo(&self, frame: &mut Frame, area: Rect, cx: usize) {
        let logo_lines = [
            "╔══════════════════════╗",
            "║  ░▒▓█ ANVIL █▓▒░    ║",
            "╚══════════════════════╝",
        ];
        // Gently pulse the logo brightness.
        let pulse  = ((self.melt_frame as f64 * 0.08).sin() * 30.0 + 200.0) as u8;
        let col    = Color::Rgb(0, pulse.min(133), 255.min(u16::from(pulse) + 100) as u8);
        let logo_h = logo_lines.len() as u16;
        let logo_w = logo_lines[0].len() as u16;
        let lx     = cx.saturating_sub(logo_w as usize / 2) as u16;
        // Place the logo above the heat waves (about h/2 - 7).
        let h  = area.height as usize;
        let ly = area.y + (h as u16 / 2).saturating_sub(logo_h + 5);

        for (i, line) in logo_lines.iter().enumerate() {
            let y = ly + i as u16;
            if y >= area.y + area.height { break; }
            let col_i = if i == 1 {
                col
            } else {
                Color::Rgb(
                    COL_IRON.as_rgb().map_or(139, |(r,_,_)| r),
                    COL_IRON.as_rgb().map_or(69, |(_,g,_)| g),
                    COL_IRON.as_rgb().map_or(19, |(_,_,b)| b),
                )
            };
            frame.render_widget(
                Paragraph::new(Span::styled(*line, Style::default().fg(col_i))),
                Rect {
                    x:      area.x + lx,
                    y,
                    width:  logo_w,
                    height: 1,
                },
            );
        }
    }

    // ─── Embers ───────────────────────────────────────────────────────────────

    fn render_embers(&self, frame: &mut Frame, area: Rect) {
        for e in &self.embers {
            let col_x = e.x.round() as i32;
            let col_y = e.y.round() as i32;
            if col_x < 0 || col_y < 0 { continue; }
            let col_x = col_x as u16;
            let col_y = col_y as u16;
            if col_x >= area.x + area.width || col_y >= area.y + area.height { continue; }
            let ax = area.x + col_x;
            let ay = area.y + col_y;
            if ax >= area.x + area.width || ay >= area.y + area.height { continue; }
            let brightness = (e.life * 255.0) as u8;
            let col = if e.life > 0.7 {
                Color::Rgb(255, brightness.min(220), 50)   // bright white-yellow
            } else if e.life > 0.4 {
                COL_EMBER                                    // yellow-orange
            } else {
                Color::Rgb(brightness, brightness / 3, 0)   // dim red
            };
            frame.render_widget(
                Paragraph::new(Span::styled(e.ch.to_string(), Style::default().fg(col))),
                Rect { x: ax, y: ay, width: 1, height: 1 },
            );
        }
    }

    // ─── Status line ──────────────────────────────────────────────────────────

    fn render_status_line(&self, frame: &mut Frame, area: Rect) {
        let status = format!(
            "  Press any key to resume  •  Anvil v{}  ",
            env!("CARGO_PKG_VERSION")
        );
        let y = area.y + area.height.saturating_sub(1);
        let w = status.len().min(area.width as usize) as u16;
        let x = area.x + (area.width.saturating_sub(w)) / 2;
        frame.render_widget(
            Paragraph::new(Span::styled(status, Style::default().fg(COL_STATUS))),
            Rect { x, y, width: w, height: 1 },
        );
    }
}

// ─── Color helper extension ───────────────────────────────────────────────────

trait ColorExt {
    fn as_rgb(self) -> Option<(u8, u8, u8)>;
}

impl ColorExt for Color {
    fn as_rgb(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }
}
