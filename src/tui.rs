use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::{Duration, Instant};

pub struct AppState {
    pub current_file: usize,
    pub total_files: usize,
    pub total_found: usize,
    pub seeded_count: usize,
    pub input_word_count: usize,
    pub start_time: Instant,
    pub queue_history: Vec<(f64, f64)>,  // (time_secs, queue_length)
    pub found_history: Vec<(f64, f64)>,  // (time_secs, total_found)
    pub optimal_ortho: Option<String>,
    pub current_queue_length: usize,
    pub processing_complete: bool,
}

impl AppState {
    pub fn new(total_files: usize) -> Self {
        Self {
            current_file: 0,
            total_files,
            total_found: 0,
            seeded_count: 0,
            input_word_count: 0,
            start_time: Instant::now(),
            queue_history: Vec::new(),
            found_history: Vec::new(),
            optimal_ortho: None,
            current_queue_length: 0,
            processing_complete: false,
        }
    }

    pub fn update_metrics(&mut self, queue_len: usize, total_found: usize) {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        self.current_queue_length = queue_len;
        self.total_found = total_found;
        self.queue_history.push((elapsed, queue_len as f64));
        self.found_history.push((elapsed, total_found as f64));
        
        // Downsample history when it gets too large to keep memory bounded
        const MAX_HISTORY_POINTS: usize = 1_000;
        if self.queue_history.len() > MAX_HISTORY_POINTS {
            // Keep every other point to reduce by half
            self.queue_history = self.queue_history.iter()
                .step_by(2)
                .copied()
                .collect();
            self.found_history = self.found_history.iter()
                .step_by(2)
                .copied()
                .collect();
        }
    }

    pub fn start_file(&mut self, file_num: usize, word_count: usize, seeded: usize) {
        self.current_file = file_num;
        self.input_word_count = word_count;
        self.seeded_count = seeded;
    }

    pub fn set_optimal(&mut self, ortho_display: String) {
        self.optimal_ortho = Some(ortho_display);
    }
    
    pub fn mark_complete(&mut self) {
        self.processing_complete = true;
    }
}

pub struct TuiApp {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TuiApp {
    pub fn new() -> Result<Self, io::Error> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        
        Ok(Self { terminal })
    }

    pub fn draw(&mut self, state: &AppState) -> Result<bool, io::Error> {
        // Check for quit before drawing
        if self.should_quit() {
            return Ok(true);
        }
        
        self.terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(6),  // Stats
                    Constraint::Min(10),    // Charts
                    Constraint::Length(10),  // Optimal ortho (increased for table)
                ])
                .split(f.area());

            // Top: Big picture stats
            render_stats(f, chunks[0], state);

            // Middle: Charts
            render_charts(f, chunks[1], state);

            // Bottom: Optimal ortho
            render_optimal(f, chunks[2], state);
        })?;
        Ok(false)
    }

    fn should_quit(&self) -> bool {
        if event::poll(Duration::from_millis(10)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                return key.code == KeyCode::Char('q') || key.code == KeyCode::Esc;
            }
        }
        false
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// Format a number in human-readable format (K, M, B style)
fn format_human(num: f64) -> String {
    if num >= 1_000_000_000.0 {
        format!("{:.1}B", num / 1_000_000_000.0)
    } else if num >= 1_000_000.0 {
        format!("{:.1}M", num / 1_000_000.0)
    } else if num >= 1_000.0 {
        format!("{:.1}K", num / 1_000.0)
    } else {
        format!("{:.0}", num)
    }
}

fn render_stats(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let elapsed = state.start_time.elapsed();
    let hours = elapsed.as_secs() / 3600;
    let minutes = (elapsed.as_secs() % 3600) / 60;
    let seconds = elapsed.as_secs() % 60;

    let mut stats_text = vec![
        Line::from(vec![
            Span::styled("File: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}/{}", state.current_file, state.total_files)),
            Span::styled("  |  Total Found: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.total_found as f64)),
            Span::styled("  |  Runtime: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{:02}:{:02}:{:02}", hours, minutes, seconds)),
        ]),
        Line::from(vec![
            Span::styled("Seeded: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.seeded_count as f64)),
            Span::styled("  |  Input Words: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.input_word_count as f64)),
            Span::styled("  |  Queue Length: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.current_queue_length as f64)),
        ]),
    ];
    
    if state.processing_complete {
        stats_text.push(Line::from(vec![
            Span::styled("STATUS: ", Style::default().fg(Color::Green)),
            Span::styled("COMPLETE - Press 'q' to quit", Style::default().fg(Color::Green)),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Statistics")
        .style(Style::default().fg(Color::White));

    let paragraph = Paragraph::new(stats_text)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(paragraph, area);
}

fn render_charts(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    // Split into two rows: Recent (top) and All-time (bottom)
    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    
    // Split each row into two columns: Queue (left) and Found (right)
    let recent_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(row_chunks[0]);
    
    let alltime_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(row_chunks[1]);

    // Render recent queue chart (top-left)
    render_queue_chart(f, recent_chunks[0], state, true);
    
    // Render recent found chart (top-right)
    render_found_chart(f, recent_chunks[1], state, true);
    
    // Render all-time queue chart (bottom-left)
    render_queue_chart(f, alltime_chunks[0], state, false);
    
    // Render all-time found chart (bottom-right)
    render_found_chart(f, alltime_chunks[1], state, false);
}

fn render_queue_chart(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState, recent_only: bool) {
    if state.queue_history.is_empty() {
        return;
    }
    
    let queue_data: Vec<(f64, f64)> = if recent_only {
        // Show last 1000 points for recent view (no downsampling)
        let start_idx = state.queue_history.len().saturating_sub(1000);
        state.queue_history[start_idx..].to_vec()
    } else {
        // For all-time view, downsample to 500-1000 points evenly spaced
        let total_points = state.queue_history.len();
        if total_points <= 1000 {
            state.queue_history.clone()
        } else {
            // Take evenly spaced points to get approximately 500-1000 points
            let step = (total_points / 500).max(1);
            state.queue_history.iter()
                .step_by(step)
                .copied()
                .collect()
        }
    };
    
    let max_queue = queue_data.iter().map(|(_, q)| *q).fold(0.0f64, f64::max);
    let min_time = queue_data.first().map(|(t, _)| *t).unwrap_or(0.0);
    let max_time = queue_data.last().map(|(t, _)| *t).unwrap_or(1.0);

    let dataset = Dataset::default()
        .name("Queue")
        .marker(symbols::Marker::Braille)
        .style(Style::default().fg(Color::Yellow))
        .data(&queue_data);

    let title = if recent_only {
        "Queue Length (Recent)"
    } else {
        "Queue Length (All Time)"
    };

    let chart = Chart::new(vec![dataset])
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White)),
        )
        .x_axis(
            Axis::default()
                .title("Time (s)")
                .style(Style::default().fg(Color::Gray))
                .bounds([min_time, max_time])
                .labels(vec![
                    Span::raw(format!("{:.0}", min_time)),
                    Span::raw(format!("{:.0}", max_time)),
                ]),
        )
        .y_axis(
            Axis::default()
                .title("Queue")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, max_queue])
                .labels(vec![
                    Span::raw("0"),
                    Span::raw(format_human(max_queue)),
                ]),
        );

    f.render_widget(chart, area);
}

fn render_found_chart(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState, recent_only: bool) {
    if state.found_history.is_empty() {
        return;
    }
    
    let found_data: Vec<(f64, f64)> = if recent_only {
        // Show last 1000 points for recent view (no downsampling)
        let start_idx = state.found_history.len().saturating_sub(1000);
        state.found_history[start_idx..].to_vec()
    } else {
        // For all-time view, downsample to 500-1000 points evenly spaced
        let total_points = state.found_history.len();
        if total_points <= 1000 {
            state.found_history.clone()
        } else {
            // Take evenly spaced points to get approximately 500-1000 points
            let step = (total_points / 500).max(1);
            state.found_history.iter()
                .step_by(step)
                .copied()
                .collect()
        }
    };
    
    let max_found = found_data.iter().map(|(_, f)| *f).fold(0.0f64, f64::max);
    let min_time = found_data.first().map(|(t, _)| *t).unwrap_or(0.0);
    let max_time = found_data.last().map(|(t, _)| *t).unwrap_or(1.0);

    let dataset = Dataset::default()
        .name("Found")
        .marker(symbols::Marker::Braille)
        .style(Style::default().fg(Color::Green))
        .data(&found_data);

    let title = if recent_only {
        "Total Found (Recent)"
    } else {
        "Total Found (All Time)"
    };

    let chart = Chart::new(vec![dataset])
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White)),
        )
        .x_axis(
            Axis::default()
                .title("Time (s)")
                .style(Style::default().fg(Color::Gray))
                .bounds([min_time, max_time])
                .labels(vec![
                    Span::raw(format!("{:.0}", min_time)),
                    Span::raw(format!("{:.0}", max_time)),
                ]),
        )
        .y_axis(
            Axis::default()
                .title("Found")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, max_found])
                .labels(vec![
                    Span::raw("0"),
                    Span::raw(format_human(max_found)),
                ]),
        );

    f.render_widget(chart, area);
}

fn render_optimal(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let text = if let Some(ref ortho) = state.optimal_ortho {
        ortho.lines()
            .map(|line| Line::from(Span::raw(line.to_string())))
            .collect::<Vec<_>>()
    } else {
        vec![Line::from(Span::styled(
            "No optimal ortho yet",
            Style::default().fg(Color::DarkGray),
        ))]
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Optimal Ortho")
        .style(Style::default().fg(Color::White));

    let paragraph = Paragraph::new(text)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(paragraph, area);
}
