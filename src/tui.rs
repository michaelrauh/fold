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
    pub queue_history_recent: Vec<(f64, f64)>,  // Recent: last 1000 points, no downsampling
    pub found_history_recent: Vec<(f64, f64)>,  // Recent: last 1000 points, no downsampling
    pub queue_history_alltime: Vec<(f64, f64)>, // All-time: progressively downsampled
    pub found_history_alltime: Vec<(f64, f64)>, // All-time: progressively downsampled
    pub optimal_ortho: Option<String>,
    pub current_queue_length: usize,
    pub processing_complete: bool,
    // Cache statistics
    pub bloom_hits: usize,
    pub bloom_misses: usize,
    pub bloom_false_positives: usize,
    pub shard_cache_hits: usize,
    pub disk_checks: usize,
    // Queue statistics
    pub queue_memory_count: usize,
    pub queue_disk_count: usize,
    // Queue rate statistics (operations per second)
    pub work_queue_disk_write_rate: f64,
    pub work_queue_disk_read_rate: f64,
    pub results_queue_disk_write_rate: f64,
    // Buffer tuning statistics
    pub work_queue_spillover_events: u64,
    pub work_queue_peak_disk: usize,
    pub work_queue_seconds_since_spillover: f64,
    pub work_queue_load_events: u64,
    pub work_queue_seconds_since_load: f64,
    pub results_queue_spillover_events: u64,
    pub results_queue_peak_disk: usize,
    pub results_queue_seconds_since_spillover: f64,
    pub results_queue_load_events: u64,
    pub results_queue_seconds_since_load: f64,
    // File processing timing
    pub last_file_start_time: Option<Instant>,
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
            queue_history_recent: Vec::new(),
            found_history_recent: Vec::new(),
            queue_history_alltime: Vec::new(),
            found_history_alltime: Vec::new(),
            optimal_ortho: None,
            current_queue_length: 0,
            processing_complete: false,
            bloom_hits: 0,
            bloom_misses: 0,
            bloom_false_positives: 0,
            shard_cache_hits: 0,
            disk_checks: 0,
            queue_memory_count: 0,
            queue_disk_count: 0,
            work_queue_disk_write_rate: 0.0,
            work_queue_disk_read_rate: 0.0,
            results_queue_disk_write_rate: 0.0,
            work_queue_spillover_events: 0,
            work_queue_peak_disk: 0,
            work_queue_seconds_since_spillover: 0.0,
            work_queue_load_events: 0,
            work_queue_seconds_since_load: 0.0,
            results_queue_spillover_events: 0,
            results_queue_peak_disk: 0,
            results_queue_seconds_since_spillover: 0.0,
            results_queue_load_events: 0,
            results_queue_seconds_since_load: 0.0,
            last_file_start_time: None,
        }
    }

    pub fn update_metrics(&mut self, queue_len: usize, total_found: usize) {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        self.current_queue_length = queue_len;
        self.total_found = total_found;
        
        let queue_point = (elapsed, queue_len as f64);
        let found_point = (elapsed, total_found as f64);
        
        // Update recent history: rolling window of last 1000 points
        self.queue_history_recent.push(queue_point);
        self.found_history_recent.push(found_point);
        if self.queue_history_recent.len() > 1000 {
            self.queue_history_recent.remove(0);
            self.found_history_recent.remove(0);
        }
        
        // Update all-time history: progressively downsample to keep around 1000 points
        self.queue_history_alltime.push(queue_point);
        self.found_history_alltime.push(found_point);
        
        if self.queue_history_alltime.len() > 1000 {
            // Downsample: keep every other point
            self.queue_history_alltime = self.queue_history_alltime.iter()
                .step_by(2)
                .copied()
                .collect();
            self.found_history_alltime = self.found_history_alltime.iter()
                .step_by(2)
                .copied()
                .collect();
        }
    }

    pub fn start_file(&mut self, file_num: usize, word_count: usize, seeded: usize) {
        self.current_file = file_num;
        self.input_word_count = word_count;
        self.seeded_count = seeded;
        self.last_file_start_time = Some(Instant::now());
    }

    pub fn set_optimal(&mut self, ortho_display: String) {
        self.optimal_ortho = Some(ortho_display);
    }
    
    pub fn mark_complete(&mut self) {
        self.processing_complete = true;
    }
    
    pub fn update_cache_stats(&mut self, bloom_hits: usize, bloom_misses: usize, bloom_false_positives: usize, shard_cache_hits: usize, disk_checks: usize, 
                              queue_mem: usize, queue_disk: usize,
                              work_write_rate: f64, work_read_rate: f64, results_write_rate: f64,
                              work_spillover: u64, work_peak: usize, work_spillover_time: f64,
                              work_loads: u64, work_load_time: f64,
                              results_spillover: u64, results_peak: usize, results_spillover_time: f64,
                              results_loads: u64, results_load_time: f64) {
        self.bloom_hits = bloom_hits;
        self.bloom_misses = bloom_misses;
        self.bloom_false_positives = bloom_false_positives;
        self.shard_cache_hits = shard_cache_hits;
        self.disk_checks = disk_checks;
        self.queue_memory_count = queue_mem;
        self.queue_disk_count = queue_disk;
        self.work_queue_disk_write_rate = work_write_rate;
        self.work_queue_disk_read_rate = work_read_rate;
        self.results_queue_disk_write_rate = results_write_rate;
        self.work_queue_spillover_events = work_spillover;
        self.work_queue_peak_disk = work_peak;
        self.work_queue_seconds_since_spillover = work_spillover_time;
        self.work_queue_load_events = work_loads;
        self.work_queue_seconds_since_load = work_load_time;
        self.results_queue_spillover_events = results_spillover;
        self.results_queue_peak_disk = results_peak;
        self.results_queue_seconds_since_spillover = results_spillover_time;
        self.results_queue_load_events = results_loads;
        self.results_queue_seconds_since_load = results_load_time;
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

/// Format time in seconds as a human-readable "time ago" string
fn format_time_ago(seconds: f64) -> String {
    if seconds == 0.0 {
        "never".to_string()
    } else if seconds < 60.0 {
        format!("{:.0}s ago", seconds)
    } else if seconds < 3600.0 {
        let minutes = seconds / 60.0;
        format!("{:.1}m ago", minutes)
    } else {
        let hours = seconds / 3600.0;
        format!("{:.1}h ago", hours)
    }
}

fn render_stats(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let elapsed = state.start_time.elapsed();
    let hours = elapsed.as_secs() / 3600;
    let minutes = (elapsed.as_secs() % 3600) / 60;
    let seconds = elapsed.as_secs() % 60;
    
    // Calculate cache hit rates
    // Bloom False Positive Rate: % of bloom "maybes" that were actually new items
    let bloom_fp_rate = if state.bloom_misses > 0 {
        (state.bloom_false_positives as f64 / state.bloom_misses as f64) * 100.0
    } else {
        0.0
    };
    
    // Shard Cache Hit Rate: % of bloom misses that hit the in-memory cache (avoiding disk)
    let shard_cache_hit_rate = if state.bloom_misses > 0 {
        (state.shard_cache_hits as f64 / state.bloom_misses as f64) * 100.0
    } else {
        0.0
    };
    
    // Format queue rates for display
    let format_rate = |rate: f64| -> String {
        if rate < 1.0 {
            format!("{:.2}/s", rate)
        } else if rate < 1000.0 {
            format!("{:.1}/s", rate)
        } else {
            format!("{:.1}K/s", rate / 1000.0)
        }
    };
    
    // Get RAM usage
    let ram_percent = get_ram_usage_percent();
    
    // Calculate time since last file started
    let time_since_last_file = if let Some(last_start) = state.last_file_start_time {
        last_start.elapsed().as_secs_f64()
    } else {
        0.0
    };

    let mut stats_text = vec![
        Line::from(vec![
            Span::styled("File: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}/{}", state.current_file, state.total_files)),
            Span::styled("  |  Runtime: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{:02}:{:02}:{:02}", hours, minutes, seconds)),
            Span::styled("  |  Last File: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_time_ago(time_since_last_file)),
            Span::styled("  |  RAM: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{:.1}%", ram_percent)),
        ]),
        Line::from(vec![
            Span::styled("Seeded: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.seeded_count as f64)),
            Span::styled("  |  Bloom FP Rate: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!("{:.1}%", bloom_fp_rate)),
            Span::styled("  |  Shard Cache Hit: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{:.1}%", shard_cache_hit_rate)),
        ]),
        Line::from(vec![
            Span::styled("Queue Mem: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.queue_memory_count as f64)),
            Span::styled("  |  Queue Disk: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.queue_disk_count as f64)),
        ]),
        Line::from(vec![
            Span::styled("Work Q Writes: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{} (last: {})", state.work_queue_spillover_events, format_time_ago(state.work_queue_seconds_since_spillover))),
            Span::styled("  |  Reads: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{} (last: {})", state.work_queue_load_events, format_time_ago(state.work_queue_seconds_since_load))),
            Span::styled("  |  Peak Disk: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.work_queue_peak_disk as f64)),
        ]),
        Line::from(vec![
            Span::styled("Results Q Writes: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{} (last: {})", state.results_queue_spillover_events, format_time_ago(state.results_queue_seconds_since_spillover))),
            Span::styled("  |  Reads: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{} (last: {})", state.results_queue_load_events, format_time_ago(state.results_queue_seconds_since_load))),
            Span::styled("  |  Peak Disk: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_human(state.results_queue_peak_disk as f64)),
        ]),
        Line::from(vec![
            Span::styled("Work Q Disk Write: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_rate(state.work_queue_disk_write_rate)),
            Span::styled("  |  Work Q Disk Read: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_rate(state.work_queue_disk_read_rate)),
            Span::styled("  |  Results Q Disk Write: ", Style::default().fg(Color::Cyan)),
            Span::raw(format_rate(state.results_queue_disk_write_rate)),
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
    let queue_data = if recent_only {
        &state.queue_history_recent
    } else {
        &state.queue_history_alltime
    };
    
    if queue_data.is_empty() {
        return;
    }
    
    // For recent: use the data as-is (already the last 1000 points, no downsampling)
    // For all-time: use the data as-is (already downsampled progressively)
    let queue_data: Vec<(f64, f64)> = queue_data.clone();
    
    // For Y-axis: use range from the displayed data
    let min_queue = queue_data.iter().map(|(_, q)| *q).fold(f64::INFINITY, f64::min);
    let max_queue = queue_data.iter().map(|(_, q)| *q).fold(0.0f64, f64::max);
    
    // For recent view, scale to actual data range; for all-time, start from 0
    let (y_min, y_max) = if recent_only {
        (min_queue * 0.95, max_queue * 1.05) // Add 5% padding
    } else {
        (0.0, max_queue)
    };
    
    // For X-axis: use the time range from the displayed data
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
                .bounds([y_min, y_max])
                .labels(vec![
                    Span::raw(format_human(y_min)),
                    Span::raw(format_human(y_max)),
                ]),
        );

    f.render_widget(chart, area);
}

fn render_found_chart(f: &mut Frame, area: ratatui::layout::Rect, state: &AppState, recent_only: bool) {
    let found_data = if recent_only {
        &state.found_history_recent
    } else {
        &state.found_history_alltime
    };
    
    if found_data.is_empty() {
        return;
    }
    
    // For recent: use the data as-is (already the last 1000 points, no downsampling)
    // For all-time: use the data as-is (already downsampled progressively)
    let found_data: Vec<(f64, f64)> = found_data.clone();
    
    // For Y-axis: use range from the displayed data
    let min_found = found_data.iter().map(|(_, f)| *f).fold(f64::INFINITY, f64::min);
    let max_found = found_data.iter().map(|(_, f)| *f).fold(0.0f64, f64::max);
    
    // For recent view, scale to actual data range; for all-time, start from 0
    let (y_min, y_max) = if recent_only {
        (min_found * 0.95, max_found * 1.05) // Add 5% padding
    } else {
        (0.0, max_found)
    };
    
    // For X-axis: use the time range from the displayed data
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
                .bounds([y_min, y_max])
                .labels(vec![
                    Span::raw(format_human(y_min)),
                    Span::raw(format_human(y_max)),
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

fn get_ram_usage_percent() -> f64 {
    #[cfg(target_os = "linux")]
    {
        use std::fs::File;
        use std::io::{BufRead, BufReader};
        
        if let Ok(file) = File::open("/proc/meminfo") {
            let reader = BufReader::new(file);
            let mut total_kb = 0u64;
            let mut available_kb = 0u64;
            
            for line in reader.lines().flatten() {
                if line.starts_with("MemTotal:") {
                    total_kb = line.split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                } else if line.starts_with("MemAvailable:") {
                    available_kb = line.split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                }
            }
            
            if total_kb > 0 {
                let used_kb = total_kb.saturating_sub(available_kb);
                return (used_kb as f64 / total_kb as f64) * 100.0;
            }
        }
        return 0.0;
    }
    
    #[cfg(not(target_os = "linux"))]
    {
        // For non-Linux systems, return 0.0
        0.0
    }
}
