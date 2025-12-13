use crate::metrics::{MetricSample, Metrics, MetricsSnapshot, StatusHistoryEntry};
use crate::spatial;
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ctrlc;
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Bar, BarChart, BarGroup, Block, Borders, Gauge, List, ListItem, Paragraph, Sparkline,
    },
};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
    }
}

pub struct Tui {
    metrics: Metrics,
    should_quit: Arc<AtomicBool>,
    log_scroll: usize,
    ortho_scroll: usize,
}

impl Tui {
    pub fn new(metrics: Metrics, should_quit: Arc<AtomicBool>) -> Self {
        Self {
            metrics,
            should_quit,
            log_scroll: 0,
            ortho_scroll: 0,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let _guard = TerminalGuard::new()?;

        // Trap SIGINT/SIGTERM to ensure the guard runs on shutdown.
        let quit_flag = Arc::clone(&self.should_quit);
        let _ = ctrlc::set_handler(move || {
            quit_flag.store(true, Ordering::Relaxed);
        });

        let result = self.run_loop(&mut terminal);

        result
    }

    fn run_loop<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        loop {
            if self.should_quit.load(Ordering::Relaxed) {
                break;
            }

            terminal.draw(|f| self.render(f))?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => {
                            self.should_quit.store(true, Ordering::Relaxed);
                            break;
                        }
                        KeyCode::Up => {
                            if self.log_scroll > 0 {
                                self.log_scroll -= 1;
                            }
                        }
                        KeyCode::Down => {
                            self.log_scroll += 1;
                        }
                        KeyCode::Left => {
                            if self.ortho_scroll > 0 {
                                self.ortho_scroll -= 1;
                            }
                        }
                        KeyCode::Right => {
                            self.ortho_scroll += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn render(&mut self, f: &mut Frame) {
        let snapshot = self.metrics.snapshot();

        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Min(15),
                Constraint::Length(7),
            ])
            .split(f.area());

        self.render_header(f, main_chunks[0], &snapshot);
        self.render_content(f, main_chunks[1], &snapshot);
        self.render_logs(f, main_chunks[2], &snapshot);
    }

    fn render_header(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;

        let mode_truncated = truncate_string(&snapshot.global.mode, 20);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let elapsed = now - snapshot.global.start_time;
        let elapsed_str = format_elapsed(elapsed);

        let ram_readable = format_bytes(snapshot.global.ram_bytes);
        let proc_ram_readable = format_bytes(snapshot.global.process_rss_bytes);
        let role_label = if snapshot.global.role.is_empty() {
            "unknown".to_string()
        } else {
            snapshot.global.role.clone()
        };

        let line1 = format!(
            "FOLD Dashboard [Role: {} │ Time: {} │ RAM Total: {} ({}%) │ RAM Proc: {}]",
            role_label,
            elapsed_str,
            ram_readable,
            snapshot.global.system_memory_percent,
            proc_ram_readable
        );
        let line2 = format!(
            "Mode: {} │ Interner: v{} │ Vocab: {}",
            mode_truncated,
            snapshot.global.interner_version,
            format_number(snapshot.global.vocab_size)
        );
        let line3 = format!(
            "Chunks: {} │ Processed: {} │ Remaining: {} │ Jobs: {} │ New orthos: {}",
            snapshot.global.total_chunks,
            snapshot.global.processed_chunks,
            snapshot.global.remaining_chunks,
            snapshot.global.distinct_jobs_count,
            format_number(snapshot.operation.new_orthos)
        );
        let fp_rate_display = format_percent(snapshot.global.bloom_fp_rate);
        let line4 = format!(
            "QBuf: {} │ Bloom: {} (~{}) │ Shards: {}/{} in mem",
            format_number(snapshot.global.queue_buffer_size),
            format_number(snapshot.global.bloom_capacity),
            fp_rate_display,
            format_number(snapshot.global.max_shards_in_memory),
            format_number(snapshot.global.num_shards)
        );

        let header_lines = vec![
            Line::from(truncate_string(&line1, max_width)),
            Line::from(truncate_string(&line2, max_width)),
            Line::from(truncate_string(&line3, max_width)),
            Line::from(truncate_string(&line4, max_width)),
        ];

        let header = Paragraph::new(header_lines).block(Block::default().borders(Borders::ALL));
        f.render_widget(header, area);
    }

    fn render_content(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.render_left_column(f, content_chunks[0], snapshot);
        self.render_right_column(f, content_chunks[1], snapshot);
    }

    fn render_left_column(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(4),
                Constraint::Min(5),
            ])
            .split(area);

        self.render_current_operation(f, left_chunks[0], snapshot);
        self.render_text_preview(f, left_chunks[1], snapshot);
        self.render_merge_progress(f, left_chunks[2], snapshot);
        self.render_optimal_ortho(f, left_chunks[3], snapshot);
        self.render_largest_archive(f, left_chunks[4], snapshot);
        self.render_provenance_tree(f, left_chunks[5], snapshot);
    }

    fn render_current_operation(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let percent_from_ratio =
            |ratio: f64| -> usize { (ratio.clamp(0.0, 1.0) * 100.0).round() as usize };

        let mut progress_ratio = if snapshot.operation.progress_total > 0 {
            snapshot.operation.progress_current as f64 / snapshot.operation.progress_total as f64
        } else {
            0.0
        };

        let mut progress_text = format!(
            "{} / {} ({}%)",
            format_number(snapshot.operation.progress_current),
            format_number(snapshot.operation.progress_total),
            percent_from_ratio(progress_ratio)
        );

        // During the larger-archive pass, treat seen growth as the best progress signal.
        if snapshot
            .operation
            .status
            .starts_with("Processing Larger Archive")
        {
            let seen_current = snapshot
                .seen_size_samples
                .last()
                .map(|s| s.value)
                .unwrap_or(0);
            let seen_peak = snapshot.global.seen_size_pk;

            if seen_peak > 0 {
                progress_ratio = seen_current as f64 / seen_peak as f64;
                progress_text = format!(
                    "Seen: {} / {} ({}%)",
                    format_number(seen_current),
                    format_number(seen_peak),
                    percent_from_ratio(progress_ratio)
                );
            }
        }

        let max_width = area.width.saturating_sub(2) as usize;
        let label_width = 12; // "Processing: "
        let available = max_width.saturating_sub(label_width);

        // Calculate elapsed time for current status
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let elapsed = now.saturating_sub(snapshot.operation.status_start_time);
        let elapsed_str = format_elapsed(elapsed);

        let lines = vec![
            Line::from(vec![
                Span::styled("Processing: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(&snapshot.operation.current_file, available)),
            ]),
            Line::from(vec![
                Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_string(&snapshot.operation.status, available.saturating_sub(8)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!(" ({})", elapsed_str),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(vec![
                Span::styled("New orthos: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format_number(snapshot.operation.new_orthos)),
            ]),
            Line::from(""),
            Line::from(vec![Span::raw(progress_text)]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Current Operation");
        let inner_area = block.inner(area);
        f.render_widget(block, area);

        let text_area = Rect {
            x: inner_area.x,
            y: inner_area.y,
            width: inner_area.width,
            height: inner_area.height.saturating_sub(1),
        };
        let gauge_area = Rect {
            x: inner_area.x,
            y: inner_area.y + text_area.height,
            width: inner_area.width,
            height: 1,
        };

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, text_area);

        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(progress_ratio.clamp(0.0, 1.0));
        f.render_widget(gauge, gauge_area);
    }

    fn render_text_preview(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;

        let lines = if snapshot.global.mode.contains("Merging") {
            // Show merge text preview (first 2 and last 2 words from each side)
            let preview_width = max_width.saturating_sub(20); // Reserve space for labels and word counts
            vec![
                Line::from(vec![
                    Span::styled("A: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_string(
                        &snapshot.merge.text_preview_a,
                        preview_width,
                    )),
                ]),
                Line::from(vec![
                    Span::styled("B: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_string(
                        &snapshot.merge.text_preview_b,
                        preview_width,
                    )),
                ]),
                Line::from(vec![
                    Span::styled("Words: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!(
                        "A:{} B:{}",
                        format_number(snapshot.merge.word_count_a),
                        format_number(snapshot.merge.word_count_b)
                    )),
                ]),
            ]
        } else {
            // Show text blob preview (first 4 and last 4 words)
            vec![
                Line::from(vec![
                    Span::styled("Preview: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_string(
                        &snapshot.operation.text_preview,
                        max_width.saturating_sub(9),
                    )),
                ]),
                Line::from(vec![
                    Span::styled("Words: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(format_number(snapshot.operation.word_count)),
                ]),
            ]
        };

        let block = Block::default().borders(Borders::ALL).title("Text Preview");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn render_merge_progress(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;

        let lines = vec![
            Line::from(vec![
                Span::styled("Completed: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", snapshot.merge.completed_merges)),
            ]),
            Line::from(vec![
                Span::styled("Current: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(
                    &snapshot.merge.current_merge,
                    max_width.saturating_sub(9),
                )),
            ]),
            Line::from(vec![
                Span::styled("Seeds: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "A:{} B:{}",
                    format_number(snapshot.merge.seed_orthos_a),
                    format_number(snapshot.merge.seed_orthos_b)
                )),
            ]),
            Line::from(vec![
                Span::styled("Queued: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "A:{} B:{}",
                    format_number(snapshot.merge.impacted_queued_a),
                    format_number(snapshot.merge.impacted_queued_b)
                )),
            ]),
            Line::from(vec![
                Span::styled("Vocab Δ: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "A:{} B:{}",
                    format_number(snapshot.merge.impacted_a),
                    format_number(snapshot.merge.impacted_b)
                )),
            ]),
            Line::from(vec![
                Span::styled("New: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format_number(snapshot.merge.new_orthos_from_merge),
                    Style::default().fg(Color::Green),
                ),
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Merge Progress");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn render_optimal_ortho(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let opt = &snapshot.optimal_ortho;
        let max_width = area.width.saturating_sub(2) as usize;

        // Format dimensions as [d1,d2,d3,...]
        let dims_str = if opt.dims.is_empty() {
            "N/A".to_string()
        } else {
            format!("{:?}", opt.dims)
        };

        // Calculate fullness percentage
        let fullness_pct = if opt.capacity > 0 {
            (opt.fullness * 100) / opt.capacity
        } else {
            0
        };

        // Calculate time since last update
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let time_since_update = if opt.last_update_time > 0 {
            now.saturating_sub(opt.last_update_time)
        } else {
            0
        };
        let time_str = format_elapsed(time_since_update);

        let lines = vec![
            Line::from(vec![
                Span::styled("Volume: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format_number(opt.volume), Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("Shape: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(&dims_str, max_width.saturating_sub(7))),
            ]),
            Line::from(vec![
                Span::styled("Filled: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{}/{} ({}%)",
                    opt.fullness, opt.capacity, fullness_pct
                )),
            ]),
            Line::from(vec![
                Span::styled("Updated: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} ago", time_str)),
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Optimal Ortho");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn render_largest_archive(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;

        let ortho_display = if snapshot.largest_archive.ortho_count == 0 {
            "N/A".to_string()
        } else {
            format_number(snapshot.largest_archive.ortho_count)
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("File: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(
                    &snapshot.largest_archive.filename,
                    max_width.saturating_sub(6),
                )),
            ]),
            Line::from(vec![
                Span::styled("Orthos: ", Style::default().fg(Color::DarkGray)),
                Span::raw(ortho_display),
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Largest Archive");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn render_provenance_tree(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;
        let max_height = area.height.saturating_sub(2) as usize;

        let tree_lines = parse_and_render_tree(&snapshot.global.current_lineage, max_width);

        let lines: Vec<Line> = tree_lines
            .into_iter()
            .take(max_height)
            .map(|s| Line::from(s))
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Provenance Tree");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn render_right_column(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(6),
                Constraint::Length(8),
                Constraint::Min(10),
            ])
            .split(area);

        self.render_queue_depth_chart(f, right_chunks[0], snapshot);
        self.render_seen_size_chart(f, right_chunks[1], snapshot);
        self.render_status_duration_chart(f, right_chunks[2], snapshot);
        self.render_optimal_ortho_display(f, right_chunks[3], snapshot);
    }

    fn render_queue_depth_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let sampled_data = sample_data(
            &snapshot.queue_depth_samples,
            area.width.saturating_sub(2) as usize,
        );
        let data: Vec<u64> = sampled_data.iter().map(|s| s.value as u64).collect();

        let (current, peak, rate) = if !snapshot.queue_depth_samples.is_empty() {
            let current = snapshot
                .queue_depth_samples
                .last()
                .map(|s| s.value)
                .unwrap_or(0);
            let peak = snapshot.global.queue_depth_pk;
            let rate = if snapshot.queue_depth_samples.len() >= 10 {
                let prev_idx = snapshot.queue_depth_samples.len().saturating_sub(10);
                let prev = snapshot.queue_depth_samples[prev_idx].value;
                (current as i64 - prev as i64) / 10
            } else {
                0
            };
            (current as u64, peak as u64, rate)
        } else {
            (0, 0, 0)
        };

        let rate_sign = if rate >= 0 { "+" } else { "-" };
        let title = format!(
            "Queue │ Cur:{} Pk:{} Δ{}{}/s",
            format_number(current as usize),
            format_number(peak as usize),
            rate_sign,
            format_number(rate.abs() as usize)
        );
        let max_width = area.width.saturating_sub(2) as usize;

        let sparkline = Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(truncate_string(&title, max_width)),
            )
            .data(&data)
            .style(Style::default().fg(Color::Yellow));

        f.render_widget(sparkline, area);
    }

    fn render_seen_size_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let sampled_data = sample_data(
            &snapshot.seen_size_samples,
            area.width.saturating_sub(2) as usize,
        );
        let baseline = sampled_data.first().map(|s| s.value).unwrap_or(0);
        let data: Vec<u64> = sampled_data
            .iter()
            .map(|s| s.value.saturating_sub(baseline) as u64)
            .collect();

        let (current, _peak, rate, baseline_raw) = if !snapshot.seen_size_samples.is_empty() {
            let current_raw = snapshot
                .seen_size_samples
                .last()
                .map(|s| s.value)
                .unwrap_or(0);
            let peak_raw = snapshot.global.seen_size_pk;
            let baseline_raw = baseline;
            let rate = if snapshot.seen_size_samples.len() >= 10 {
                let prev_idx = snapshot.seen_size_samples.len().saturating_sub(10);
                let prev = snapshot.seen_size_samples[prev_idx].value;
                (current_raw as i64 - prev as i64) / 10
            } else {
                0
            };
            (
                current_raw as u64,
                peak_raw as u64,
                rate,
                baseline_raw as u64,
            )
        } else {
            (0, 0, 0, 0)
        };

        let rate_sign = if rate >= 0 { "+" } else { "-" };
        let title = format!(
            "Seen │ Cur:{} Base:{} Δ{}{}/s",
            format_number(current as usize),
            format_number(baseline_raw as usize),
            rate_sign,
            format_number(rate.abs() as usize)
        );
        let max_width = area.width.saturating_sub(2) as usize;

        let sparkline = Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(truncate_string(&title, max_width)),
            )
            .data(&data)
            .style(Style::default().fg(Color::Green));

        f.render_widget(sparkline, area);
    }

    fn render_status_duration_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let stats = &snapshot.status_duration_stats;

        // Calculate statistics
        let avg = if stats.total_count > 0 {
            stats.total_duration / stats.total_count as u64
        } else {
            0
        };
        let min = if stats.total_count > 0 {
            stats.min_duration
        } else {
            0
        };
        let max = if stats.total_count > 0 {
            stats.max_duration
        } else {
            0
        };

        // Format title with statistics
        let title = format!(
            "Status Duration │ Avg:{} Min:{} Max:{}",
            format_elapsed(avg),
            format_elapsed(min),
            format_elapsed(max)
        );

        if snapshot.status_history.is_empty() {
            let block = Block::default().borders(Borders::ALL).title(title);
            f.render_widget(block, area);
            return;
        }

        // Take last N entries that fit in the available width
        let max_bars = (area.width.saturating_sub(2) / 2).max(1) as usize;
        let entries: Vec<&StatusHistoryEntry> = snapshot
            .status_history
            .iter()
            .rev()
            .take(max_bars)
            .collect();

        // Reverse back to chronological order for display
        let entries: Vec<&StatusHistoryEntry> = entries.into_iter().rev().collect();

        // Build bar data without labels or text values
        let bars: Vec<Bar> = entries
            .iter()
            .map(|entry| {
                Bar::default()
                    .value(entry.duration)
                    .text_value(String::new())
                    .style(Style::default().fg(Color::Cyan))
            })
            .collect();

        let max_duration = entries.iter().map(|e| e.duration).max().unwrap_or(1);
        let bar_group = BarGroup::default().bars(&bars);

        let chart = BarChart::default()
            .block(Block::default().borders(Borders::ALL).title(title))
            .data(bar_group)
            .bar_width(1)
            .bar_gap(0)
            .max(max_duration);

        f.render_widget(chart, area);
    }

    fn render_logs(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;

        let logs: Vec<ListItem> = snapshot
            .logs
            .iter()
            .rev()
            .skip(self.log_scroll)
            .take(area.height.saturating_sub(2) as usize)
            .map(|entry| {
                let time = format_timestamp(entry.timestamp);
                let time_padded = format!("{:8}", time);
                ListItem::new(Line::from(truncate_string(
                    &format!("{} {}", time_padded, entry.message),
                    max_width,
                )))
            })
            .collect();

        let title = truncate_string(
            "Logs (↑/↓ scroll logs, ←/→ scroll ortho, q quit)",
            max_width,
        );
        let list = List::new(logs).block(Block::default().borders(Borders::ALL).title(title));

        f.render_widget(list, area);
    }

    fn render_optimal_ortho_display(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let opt = &snapshot.optimal_ortho;
        let max_width = area.width.saturating_sub(2) as usize;
        let max_height = area.height.saturating_sub(2) as usize;

        // If no ortho data, show placeholder
        if opt.dims.is_empty() || opt.payload.is_empty() {
            let lines = vec![Line::from("No optimal ortho yet")];
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Optimal Ortho Display");
            let inner = block.inner(area);
            f.render_widget(block, area);
            let paragraph = Paragraph::new(lines);
            f.render_widget(paragraph, inner);
            return;
        }

        // Format the ortho display with column layout
        let display_lines =
            self.format_ortho_display(&opt.dims, &opt.payload, &opt.vocab, max_width, max_height);

        // Apply scrolling
        let lines: Vec<Line> = display_lines
            .into_iter()
            .skip(self.ortho_scroll)
            .take(max_height)
            .map(|s| Line::from(s))
            .collect();

        let title = "Optimal Ortho Display (←/→ scroll)";
        let block = Block::default().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn format_ortho_display(
        &self,
        dims: &[usize],
        payload: &[Option<usize>],
        vocab: &[String],
        max_width: usize,
        max_height: usize,
    ) -> Vec<String> {
        if dims.len() < 2 {
            return vec!["Invalid dimensions".to_string()];
        }

        let rows = dims[dims.len() - 2];
        let cols = dims[dims.len() - 1];
        let higher_dims = &dims[..dims.len() - 2];

        // Use spatial module to get proper coordinate mapping
        let location_to_index = spatial::get_location_to_index(dims);

        // Calculate max token width
        let max_token_width = payload
            .iter()
            .filter_map(|&opt| opt)
            .filter_map(|idx| vocab.get(idx))
            .map(|s| s.len())
            .max()
            .unwrap_or(1)
            .max(4)
            .min(10); // Cap at 10 to avoid overflow

        let format_cell = |token_id: Option<usize>| -> String {
            token_id
                .and_then(|id| vocab.get(id))
                .map(|s| {
                    format!(
                        "{:>width$}",
                        truncate_string(s, max_token_width),
                        width = max_token_width
                    )
                })
                .unwrap_or_else(|| format!("{:>width$}", "·", width = max_token_width))
        };

        let format_2d_slice = |prefix: &[usize]| -> Vec<String> {
            (0..rows)
                .map(|row| {
                    let row_str = (0..cols)
                        .map(|col| {
                            let coords: Vec<usize> =
                                prefix.iter().copied().chain([row, col]).collect();
                            location_to_index
                                .get(&coords)
                                .and_then(|&idx| payload.get(idx))
                                .and_then(|&opt| opt)
                                .map(|token_id| format_cell(Some(token_id)))
                                .unwrap_or_else(|| format_cell(None))
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    row_str
                })
                .collect()
        };

        if higher_dims.is_empty() {
            return format_2d_slice(&[]);
        }

        // Generate all possible coordinate combinations for higher dimensions
        fn generate_coords(dims: &[usize], current: Vec<usize>, all: &mut Vec<Vec<usize>>) {
            if current.len() == dims.len() {
                all.push(current);
                return;
            }
            let dim_idx = current.len();
            for i in 0..dims[dim_idx] {
                let mut next = current.clone();
                next.push(i);
                generate_coords(dims, next, all);
            }
        }

        let mut all_coords = Vec::new();
        generate_coords(higher_dims, Vec::new(), &mut all_coords);

        // Format each tile with header and content
        struct Tile {
            header: String,
            lines: Vec<String>,
        }

        let tiles: Vec<Tile> = all_coords
            .iter()
            .map(|coords| {
                let coord_str = coords
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| format!("d{}={}", i, c))
                    .collect::<Vec<_>>()
                    .join(", ");
                let header = format!("[{}]", coord_str);
                let lines = format_2d_slice(coords);
                Tile { header, lines }
            })
            .collect();

        if tiles.is_empty() {
            return vec!["No tiles".to_string()];
        }

        // Calculate dimensions of a single tile
        let tile_height = tiles[0].lines.len() + 1; // +1 for header
        let tile_width = tiles[0]
            .lines
            .iter()
            .map(|l| l.len())
            .max()
            .unwrap_or(0)
            .max(tiles[0].header.len());

        // Determine column layout
        let col_spacing = 2;
        let tiles_per_row = ((max_width + col_spacing) / (tile_width + col_spacing)).max(1);

        // Check if tiles fit in columns within available height
        let num_tile_rows = (tiles.len() + tiles_per_row - 1) / tiles_per_row;
        let total_height = num_tile_rows * tile_height;

        if tiles_per_row == 1 || total_height > max_height * 3 {
            // Too tall even with columns, or only one column fits - just stack vertically
            let mut result = Vec::new();
            for tile in tiles {
                result.push(tile.header);
                result.extend(tile.lines);
                result.push("".to_string());
            }
            result
        } else {
            // Arrange in columns
            let mut result = vec![String::new(); tile_height * num_tile_rows];

            for (tile_idx, tile) in tiles.iter().enumerate() {
                let row_idx = tile_idx / tiles_per_row;
                let col_idx = tile_idx % tiles_per_row;
                let base_line = row_idx * tile_height;
                let x_offset = col_idx * (tile_width + col_spacing);

                // Add header
                if base_line < result.len() {
                    let padded_header = format!("{:<width$}", tile.header, width = tile_width);
                    let current_len = result[base_line].len();
                    if current_len < x_offset {
                        result[base_line].push_str(&" ".repeat(x_offset - current_len));
                    }
                    if result[base_line].len() == x_offset {
                        result[base_line].push_str(&padded_header);
                    }
                }

                // Add tile lines
                for (i, line) in tile.lines.iter().enumerate() {
                    let line_idx = base_line + 1 + i;
                    if line_idx < result.len() {
                        let padded_line = format!("{:<width$}", line, width = tile_width);
                        let current_len = result[line_idx].len();
                        if current_len < x_offset {
                            result[line_idx].push_str(&" ".repeat(x_offset - current_len));
                        }
                        if result[line_idx].len() == x_offset {
                            result[line_idx].push_str(&padded_line);
                        }
                    }
                }
            }

            result
        }
    }
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s.chars().take(max_len).collect()
    } else {
        let mut result: String = s.chars().take(max_len - 3).collect();
        result.push_str("...");
        result
    }
}

fn format_number(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_percent(p: f64) -> String {
    if p.is_finite() {
        format!("{:.2}%", p * 100.0)
    } else {
        "n/a".to_string()
    }
}

fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn format_elapsed(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        format!("{}h{}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m{}s", minutes, secs)
    } else {
        format!("{}s", secs)
    }
}

fn format_timestamp(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let elapsed = now.saturating_sub(ts);

    if elapsed < 60 {
        format!("{}s ago", elapsed)
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else {
        format!("{}h ago", elapsed / 3600)
    }
}

#[derive(Debug, Clone)]
enum TreeNode {
    Leaf(String),
    Branch(Box<TreeNode>, Box<TreeNode>),
}

fn parse_lineage(s: &str) -> TreeNode {
    let trimmed = s.trim();

    if trimmed.is_empty() {
        return TreeNode::Leaf("<empty>".to_string());
    }

    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        let content = &trimmed[1..trimmed.len() - 1];
        return TreeNode::Leaf(content.to_string());
    }

    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        let inner = &trimmed[1..trimmed.len() - 1];

        let (left, right) = split_s_expression(inner);
        let left_node = parse_lineage(left);
        let right_node = parse_lineage(right);

        return TreeNode::Branch(Box::new(left_node), Box::new(right_node));
    }

    TreeNode::Leaf(trimmed.to_string())
}

fn split_s_expression(s: &str) -> (&str, &str) {
    let mut depth = 0;
    let mut in_quotes = false;

    for (i, ch) in s.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => depth += 1,
            ')' if !in_quotes => depth -= 1,
            ' ' if !in_quotes && depth == 0 => {
                return (&s[..i], &s[i + 1..]);
            }
            _ => {}
        }
    }

    (s, "")
}

fn count_nodes(node: &TreeNode) -> usize {
    match node {
        TreeNode::Leaf(_) => 1,
        TreeNode::Branch(left, right) => count_nodes(left) + count_nodes(right),
    }
}

fn tree_depth(node: &TreeNode) -> usize {
    match node {
        TreeNode::Leaf(_) => 1,
        TreeNode::Branch(left, right) => 1 + tree_depth(left).max(tree_depth(right)),
    }
}

fn parse_and_render_tree(lineage: &str, _max_width: usize) -> Vec<String> {
    if lineage.is_empty() {
        return vec!["<no lineage>".to_string()];
    }

    let tree = parse_lineage(lineage);
    let node_count = count_nodes(&tree);
    let depth = tree_depth(&tree);

    // If we got a single leaf node that looks like an S-expression, parsing failed
    if node_count == 1 {
        if let TreeNode::Leaf(content) = &tree {
            if content.starts_with('(') && content.contains(')') {
                // Parsing failed - show simple metadata
                return vec![
                    "Unparsed lineage".to_string(),
                    format!("Length: {} chars", content.len()),
                ];
            }
        }
    }

    // Render the tree
    render_tree_summary(&tree, node_count, depth)
}

fn render_tree_summary(tree: &TreeNode, node_count: usize, depth: usize) -> Vec<String> {
    let mut lines = vec![format!("Tree: {} nodes, {} levels", node_count, depth)];

    let mut level_counts = vec![0; depth];
    count_nodes_per_level(tree, 0, &mut level_counts);

    // Calculate average width
    let total_width: usize = level_counts.iter().sum();
    let avg_width = if depth > 0 {
        total_width as f64 / depth as f64
    } else {
        0.0
    };

    let leaves = count_leaves(tree);
    lines.push(format!("Leaves: {} | Avg width: {:.1}", leaves, avg_width));

    // Show sparkline for bottom 120 levels (most recent merges)
    let sparkline_len = 120.min(depth);
    let start_level = depth.saturating_sub(sparkline_len);
    let sparkline_data = &level_counts[start_level..];

    if !sparkline_data.is_empty() {
        let max_count = *sparkline_data.iter().max().unwrap_or(&1);
        let sparkline_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

        let sparkline: String = sparkline_data
            .iter()
            .map(|&count| {
                if count == 0 {
                    ' '
                } else {
                    let index = ((count as f64 / max_count as f64)
                        * (sparkline_chars.len() - 1) as f64)
                        .round() as usize;
                    sparkline_chars[index.min(sparkline_chars.len() - 1)]
                }
            })
            .collect();

        if depth > sparkline_len {
            lines.push(format!("Bottom {} levels:", sparkline_len));
        }
        lines.push(sparkline);
    }

    lines
}

fn count_nodes_per_level(node: &TreeNode, level: usize, counts: &mut [usize]) {
    if level >= counts.len() {
        return;
    }

    counts[level] += 1;

    if let TreeNode::Branch(left, right) = node {
        count_nodes_per_level(left, level + 1, counts);
        count_nodes_per_level(right, level + 1, counts);
    }
}

fn count_leaves(node: &TreeNode) -> usize {
    match node {
        TreeNode::Leaf(_) => 1,
        TreeNode::Branch(left, right) => count_leaves(left) + count_leaves(right),
    }
}

fn sample_data(samples: &[MetricSample], max_points: usize) -> Vec<MetricSample> {
    if samples.len() <= max_points {
        return samples.to_vec();
    }

    let step = samples.len() as f64 / max_points as f64;
    let mut result = Vec::with_capacity(max_points);

    for i in 0..max_points {
        let idx = (i as f64 * step) as usize;
        if idx < samples.len() {
            result.push(samples[idx].clone());
        }
    }

    result
}
