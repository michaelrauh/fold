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
    pub last_checkpoint_time: Instant,
    pub orthos_processed: usize,
}

impl AppState {
    pub fn new(total_files: usize) -> Self {
        let now = Instant::now();
        Self {
            current_file: 0,
            total_files,
            total_found: 0,
            seeded_count: 0,
            input_word_count: 0,
            start_time: now,
            queue_history: Vec::new(),
            found_history: Vec::new(),
            optimal_ortho: None,
            current_queue_length: 0,
            last_checkpoint_time: now,
            orthos_processed: 0,
        }
    }

    pub fn update_metrics(&mut self, queue_len: usize, total_found: usize) {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        self.current_queue_length = queue_len;
        self.total_found = total_found;
        self.queue_history.push((elapsed, queue_len as f64));
        self.found_history.push((elapsed, total_found as f64));
        
        // Keep only last 1000 points to avoid memory bloat
        if self.queue_history.len() > 1000 {
            self.queue_history.remove(0);
            self.found_history.remove(0);
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

    pub fn checkpoint(&mut self) {
        self.last_checkpoint_time = Instant::now();
        self.orthos_processed = 0;
    }

    pub fn increment_orthos_processed(&mut self) {
        self.orthos_processed += 1;
    }

    pub fn seconds_since_checkpoint(&self) -> u64 {
        self.last_checkpoint_time.elapsed().as_secs()
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
                    Constraint::Length(7),  // Stats (increased for extra line)
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

fn render_stats(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let elapsed = state.start_time.elapsed();
    let hours = elapsed.as_secs() / 3600;
    let minutes = (elapsed.as_secs() % 3600) / 60;
    let seconds = elapsed.as_secs() % 60;
    let since_checkpoint = state.seconds_since_checkpoint();

    let stats_text = vec![
        Line::from(vec![
            Span::styled("File: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}/{}", state.current_file, state.total_files)),
            Span::styled("  |  Total Found: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", state.total_found)),
            Span::styled("  |  Runtime: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{:02}:{:02}:{:02}", hours, minutes, seconds)),
        ]),
        Line::from(vec![
            Span::styled("Seeded: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", state.seeded_count)),
            Span::styled("  |  Input Words: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", state.input_word_count)),
            Span::styled("  |  Queue Length: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", state.current_queue_length)),
        ]),
        Line::from(vec![
            Span::styled("Orthos Processed: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", state.orthos_processed)),
            Span::styled("  |  Since Checkpoint: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}s", since_checkpoint)),
        ]),
    ];

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
    let chart_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Queue length chart
    if !state.queue_history.is_empty() {
        let queue_data: Vec<(f64, f64)> = state.queue_history.clone();
        let max_queue = queue_data.iter().map(|(_, q)| *q).fold(0.0f64, f64::max);
        let min_time = queue_data.first().map(|(t, _)| *t).unwrap_or(0.0);
        let max_time = queue_data.last().map(|(t, _)| *t).unwrap_or(1.0);

        let dataset = Dataset::default()
            .name("Queue")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Yellow))
            .data(&queue_data);

        let chart = Chart::new(vec![dataset])
            .block(
                Block::default()
                    .title("Queue Length Over Time")
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
                    .title("Queue Length")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([0.0, max_queue])
                    .labels(vec![
                        Span::raw("0"),
                        Span::raw(format!("{:.0}", max_queue)),
                    ]),
            );

        f.render_widget(chart, chart_chunks[0]);
    }

    // Total found chart
    if !state.found_history.is_empty() {
        let found_data: Vec<(f64, f64)> = state.found_history.clone();
        let max_found = found_data.iter().map(|(_, f)| *f).fold(0.0f64, f64::max);
        let min_time = found_data.first().map(|(t, _)| *t).unwrap_or(0.0);
        let max_time = found_data.last().map(|(t, _)| *t).unwrap_or(1.0);

        let dataset = Dataset::default()
            .name("Found")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Green))
            .data(&found_data);

        let chart = Chart::new(vec![dataset])
            .block(
                Block::default()
                    .title("Total Found Over Time")
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
                    .title("Total Found")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([0.0, max_found])
                    .labels(vec![
                        Span::raw("0"),
                        Span::raw(format!("{:.0}", max_found)),
                    ]),
            );

        f.render_widget(chart, chart_chunks[1]);
    }
}

fn render_optimal(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let text = if let Some(ref ortho) = state.optimal_ortho {
        // Split the ortho string into multiple lines
        ortho.lines()
            .map(|line| Line::from(Span::raw(line.to_string())))
            .collect()
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
