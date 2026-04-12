use crate::metrics::{MetricSample, Metrics, MetricsSnapshot, StatusHistoryEntry};
use crate::ortho::{Dim, PayloadVal, dim_to_usize, payload_to_usize};
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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
    snapshot_path: Option<PathBuf>,
    last_snapshot_write: Instant,
}

impl Tui {
    pub fn new(
        metrics: Metrics,
        should_quit: Arc<AtomicBool>,
        snapshot_path: Option<PathBuf>,
    ) -> Self {
        Self {
            metrics,
            should_quit,
            log_scroll: 0,
            ortho_scroll: 0,
            snapshot_path,
            last_snapshot_write: Instant::now(),
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
            self.maybe_persist_snapshot()?;

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

    fn maybe_persist_snapshot(&mut self) -> io::Result<()> {
        let Some(path) = &self.snapshot_path else {
            return Ok(());
        };

        const SNAPSHOT_INTERVAL_SECS: u64 = 3;
        if self.last_snapshot_write.elapsed() < Duration::from_secs(SNAPSHOT_INTERVAL_SECS) {
            return Ok(());
        }

        let snapshot = self.metrics.snapshot();
        let contents = format_snapshot(&snapshot);
        if let Err(err) = std::fs::write(path, contents) {
            self.metrics
                .add_log(format!("TUI snapshot write failed: {}", err));
            // Do not fail the loop on snapshot write issues.
            return Ok(());
        }
        self.last_snapshot_write = Instant::now();
        Ok(())
    }

    fn render(&mut self, f: &mut Frame) {
        let snapshot = self.metrics.snapshot();

        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7),
                Constraint::Min(15),
                Constraint::Length(6),
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
        let proc_cap_readable = format_bytes(snapshot.global.process_rss_cap_bytes);
        let role_label = if snapshot.global.role.is_empty() {
            "unknown".to_string()
        } else {
            snapshot.global.role.clone()
        };

        let line1 = format!(
            "FOLD Dashboard [Role: {} │ Time: {} │ RAM Total: {} ({}%) │ RAM Proc: {} / {}]",
            role_label,
            elapsed_str,
            ram_readable,
            snapshot.global.system_memory_percent,
            proc_ram_readable,
            proc_cap_readable
        );
        let line2 = format!(
            "Mode: {} │ Interner: v{} │ Vocab: {} │ Chunks:{} Proc:{} Rem:{} Jobs:{}",
            mode_truncated,
            snapshot.global.interner_version,
            format_number(snapshot.global.vocab_size),
            format_number(snapshot.global.total_chunks),
            format_number(snapshot.global.processed_chunks),
            format_number(snapshot.global.remaining_chunks),
            format_number(snapshot.global.distinct_jobs_count)
        );
        let pruned = snapshot.operation.pruned_completions;
        let expanded = snapshot.operation.expanded_completions;
        let ratio = if (pruned + expanded) == 0 {
            0.0
        } else {
            (pruned as f64) / ((pruned + expanded) as f64)
        };
        let disk_line = if snapshot.global.disk_total_bytes > 0 {
            let total = snapshot.global.disk_total_bytes;
            let free = snapshot.global.disk_available_bytes;
            let pct = if total > 0 {
                (free as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            format!(
                "Disk: {} free / {} ({:.0}%)",
                format_bytes(free as usize),
                format_bytes(total as usize),
                pct
            )
        } else {
            "Disk: n/a".to_string()
        };
        let comp_line = if snapshot.global.compression_uncompressed_bytes > 0 {
            let unc = snapshot.global.compression_uncompressed_bytes;
            let comp = snapshot.global.compression_compressed_bytes;
            let ratio = if comp > 0 {
                unc as f64 / comp as f64
            } else {
                0.0
            };
            let saved = unc.saturating_sub(comp);
            format!("{:.2}× saved {}", ratio, format_bytes(saved as usize))
        } else {
            "n/a".to_string()
        };

        let line3 = format!(
            "Gen:{} │ Phase:{} │ Work:{} │ Accepted:{} │ New:{}",
            snapshot.global.generation,
            truncate_string(&snapshot.global.phase, 28),
            format_number(snapshot.global.work_len as usize),
            format_number(snapshot.global.seen_len_accepted as usize),
            format_number(snapshot.operation.new_orthos)
        );
        let line4 = format!(
            "Arena:{} │ Work:{} / {} │ Seg:{} / {} │ Fan-in:{} │ Prune:{:.1}%",
            format_bytes(snapshot.global.compaction_arena_cap_bytes),
            format_bytes(snapshot.global.work_cache_bytes),
            format_bytes(snapshot.global.work_cache_cap_bytes),
            format_bytes(snapshot.global.segment_batch_bytes),
            format_bytes(snapshot.global.segment_batch_cap_bytes),
            snapshot.global.fan_in,
            ratio * 100.0
        );
        let line5 = format!(
            "{} │ Comp:{} │ Off:{} {} │ Dl:{} {} │ Pressure:{}",
            disk_line,
            comp_line,
            format_number(snapshot.global.offloaded_files as usize),
            format_bytes(snapshot.global.offloaded_bytes as usize),
            format_number(snapshot.global.downloaded_files as usize),
            format_bytes(snapshot.global.downloaded_bytes as usize),
            format_number(snapshot.global.pressure_triggers as usize)
        );
        let header_lines = vec![
            Line::from(truncate_string(&line1, max_width)),
            Line::from(truncate_string(&line2, max_width)),
            Line::from(truncate_string(&line3, max_width)),
            Line::from(truncate_string(&line4, max_width)),
            Line::from(truncate_string(&line5, max_width)),
        ];

        let header = Paragraph::new(header_lines).block(Block::default().borders(Borders::ALL));
        f.render_widget(header, area);
    }

    fn render_content(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(area);

        self.render_left_column(f, content_chunks[0], snapshot);
        self.render_right_column(f, content_chunks[1], snapshot);
    }

    fn render_left_column(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let constraints = if area.height >= 24 {
            vec![
                Constraint::Length(9),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Min(5),
            ]
        } else {
            vec![
                Constraint::Length(9),
                Constraint::Length(5),
                Constraint::Length(5),
                Constraint::Min(2),
            ]
        };

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        self.render_current_merge(f, left_chunks[0], snapshot);
        self.render_optimal_ortho(f, left_chunks[1], snapshot);
        self.render_pruning_archive(f, left_chunks[2], snapshot);
        self.render_provenance_summary(f, left_chunks[3], snapshot);
    }

    fn render_current_merge(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
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
                .landing_buffer_samples
                .last()
                .map(|s| s.value)
                .unwrap_or(0);
            let seen_peak = snapshot
                .landing_buffer_samples
                .iter()
                .map(|s| s.value)
                .max()
                .unwrap_or(0);

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
        let available = max_width.saturating_sub(12);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let elapsed = now.saturating_sub(snapshot.operation.status_start_time);
        let elapsed_str = format_elapsed(elapsed);

        let mut lines = vec![
            Line::from(vec![
                Span::styled("Processing: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(
                    &format!(
                        "{} │ {}",
                        snapshot.operation.current_file, snapshot.merge.current_merge
                    ),
                    available,
                )),
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
        ];

        if snapshot.global.mode.contains("Merging") {
            lines.extend([
                Line::from(vec![
                    Span::styled("A: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_string(
                        &snapshot.merge.text_preview_a,
                        max_width.saturating_sub(3),
                    )),
                ]),
                Line::from(vec![
                    Span::styled("B: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_string(
                        &snapshot.merge.text_preview_b,
                        max_width.saturating_sub(3),
                    )),
                ]),
                Line::from(truncate_string(
                    &format!(
                        "Words A:{} B:{} │ Seeds A:{} B:{}",
                        format_number(snapshot.merge.word_count_a),
                        format_number(snapshot.merge.word_count_b),
                        format_number(snapshot.merge.seed_orthos_a),
                        format_number(snapshot.merge.seed_orthos_b)
                    ),
                    max_width,
                )),
                Line::from(truncate_string(
                    &format!(
                        "Queued A:{} B:{} │ ΔVoc A:{} B:{}",
                        format_number(snapshot.merge.impacted_queued_a),
                        format_number(snapshot.merge.impacted_queued_b),
                        format_number(snapshot.merge.impacted_a),
                        format_number(snapshot.merge.impacted_b)
                    ),
                    max_width,
                )),
            ]);
        } else {
            lines.extend([
                Line::from(vec![
                    Span::styled("Preview: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_string(
                        &snapshot.operation.text_preview,
                        max_width.saturating_sub(9),
                    )),
                ]),
                Line::from(truncate_string(
                    &format!(
                        "Words:{} │ New:{}",
                        format_number(snapshot.operation.word_count),
                        format_number(snapshot.operation.new_orthos)
                    ),
                    max_width,
                )),
            ]);
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Current Merge");
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
            .label(truncate_string(&progress_text, inner_area.width as usize))
            .ratio(progress_ratio.clamp(0.0, 1.0));
        f.render_widget(gauge, gauge_area);
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
        let variance = if opt.variance_den > 0 {
            opt.variance_num as f64 / opt.variance_den as f64
        } else {
            0.0
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
                Span::styled("Vol: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format_number(opt.volume), Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("Shape: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(&dims_str, max_width.saturating_sub(7))),
            ]),
            Line::from(vec![
                Span::styled("Var: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{variance:.3}")),
            ]),
            Line::from(vec![
                Span::styled("Full: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{}/{} ({}%)",
                    opt.fullness, opt.capacity, fullness_pct
                )),
            ]),
            Line::from(vec![
                Span::styled("Upd: ", Style::default().fg(Color::DarkGray)),
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

    fn render_pruning_archive(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;
        let pruned = snapshot.operation.pruned_completions;
        let expanded = snapshot.operation.expanded_completions;
        let ratio = if pruned + expanded == 0 {
            0.0
        } else {
            pruned as f64 / (pruned + expanded) as f64
        };
        let current_line = format!(
            "Current: pruned {} / expanded {} ({:.1}%)",
            format_number(pruned),
            format_number(expanded),
            ratio * 100.0
        );

        let history = snapshot
            .prune_history
            .iter()
            .rev()
            .take(5)
            .rev()
            .map(|s| {
                let r = if s.pruned + s.expanded == 0 {
                    0.0
                } else {
                    s.pruned as f64 / (s.pruned + s.expanded) as f64
                };
                format!("G{} {:.0}%", s.generation, r * 100.0)
            })
            .collect::<Vec<_>>()
            .join(" │ ");
        let history_line = if history.is_empty() {
            "History: n/a".to_string()
        } else {
            format!(
                "History: {}",
                truncate_string(&history, max_width.saturating_sub(9))
            )
        };

        let comp_kept = snapshot.merge.compaction_kept;
        let comp_pruned = snapshot.merge.compaction_pruned;
        let comp_line = if comp_kept + comp_pruned == 0 {
            "Comp: n/a".to_string()
        } else {
            let pct = if comp_kept + comp_pruned == 0 {
                0.0
            } else {
                comp_pruned as f64 / (comp_kept + comp_pruned) as f64
            };
            format!(
                "Comp: kept {} pruned {} ({:.1}%)",
                format_number(comp_kept),
                format_number(comp_pruned),
                pct * 100.0
            )
        };

        let largest_path = if snapshot.largest_archive.filename.is_empty() {
            "Largest: n/a".to_string()
        } else {
            format!(
                "Largest: {}",
                truncate_string(
                    &snapshot.largest_archive.filename,
                    max_width.saturating_sub(9),
                )
            )
        };
        let largest_count = format!(
            "Count: {}",
            if snapshot.largest_archive.ortho_count == 0 {
                "n/a".to_string()
            } else {
                format_number(snapshot.largest_archive.ortho_count)
            }
        );

        let lines = vec![
            Line::from(truncate_string(&current_line, max_width)),
            Line::from(history_line),
            Line::from(truncate_string(&comp_line, max_width)),
            Line::from(truncate_string(&largest_path, max_width)),
            Line::from(truncate_string(&largest_count, max_width)),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Pruning & Archive");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn render_provenance_summary(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let max_width = area.width.saturating_sub(2) as usize;
        let max_height = area.height.saturating_sub(2) as usize;

        let tree_lines = parse_and_render_tree(&snapshot.global.current_lineage, max_width)
            .into_iter()
            .filter(|line| !line.starts_with("Bottom "))
            .collect::<Vec<_>>();

        let lines: Vec<Line> = tree_lines
            .into_iter()
            .take(max_height)
            .map(|s| Line::from(s))
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Provenance Summary");
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
                Constraint::Length(5),
                Constraint::Length(11),
                Constraint::Min(8),
            ])
            .split(area);

        self.render_work_depth_chart(f, right_chunks[0], snapshot);
        self.render_seen_growth_chart(f, right_chunks[1], snapshot);
        self.render_history_panel(f, right_chunks[2], snapshot);
        self.render_optimal_ortho_display(f, right_chunks[3], snapshot);
    }

    fn render_work_depth_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let sampled_data = sample_data(
            &snapshot.work_len_samples,
            area.width.saturating_sub(2) as usize,
        );
        let data: Vec<u64> = sampled_data.iter().map(|s| s.value as u64).collect();

        let (current, peak, rate) = if !snapshot.work_len_samples.is_empty() {
            let current = snapshot
                .work_len_samples
                .last()
                .map(|s| s.value)
                .unwrap_or(0);
            let peak = snapshot
                .work_len_samples
                .iter()
                .map(|s| s.value)
                .max()
                .unwrap_or(0);
            let rate = if snapshot.work_len_samples.len() >= 10 {
                let prev_idx = snapshot.work_len_samples.len().saturating_sub(10);
                let prev = snapshot.work_len_samples[prev_idx].value;
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
            "Work │ Cur:{} Pk:{} Δ{}{}",
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

    fn render_seen_growth_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let sampled_data = sample_data(
            &snapshot.landing_buffer_samples,
            area.width.saturating_sub(2) as usize,
        );
        let baseline = sampled_data.first().map(|s| s.value).unwrap_or(0);
        let data: Vec<u64> = sampled_data
            .iter()
            .map(|s| s.value.saturating_sub(baseline) as u64)
            .collect();

        let (current, rate, baseline_raw) = if !snapshot.landing_buffer_samples.is_empty() {
            let current_raw = snapshot
                .landing_buffer_samples
                .last()
                .map(|s| s.value)
                .unwrap_or(0);
            let baseline_raw = baseline;
            let rate = if snapshot.landing_buffer_samples.len() >= 10 {
                let prev_idx = snapshot.landing_buffer_samples.len().saturating_sub(10);
                let prev = snapshot.landing_buffer_samples[prev_idx].value;
                (current_raw as i64 - prev as i64) / 10
            } else {
                0
            };
            (current_raw as u64, rate, baseline_raw as u64)
        } else {
            (0, 0, 0)
        };

        let rate_sign = if rate >= 0 { "+" } else { "-" };
        let title = format!(
            "Produced │ Cur:{} Base:{} Δ{}{}",
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

    fn render_history_panel(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        // Display generational store with per-bucket visualization
        let phase_str = &snapshot.global.phase;
        let generation = snapshot.global.generation;
        let work_len = snapshot.global.work_len;
        let seen = snapshot.global.seen_len_accepted;

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Generational Store");
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Check if we're in a transition (processing buckets)
        let in_transition = snapshot.bucket_metrics.iter().any(|b| {
            !matches!(
                b.state,
                crate::metrics::BucketState::Pending
                    | crate::metrics::BucketState::Complete
                    | crate::metrics::BucketState::Empty
            )
        });

        if in_transition && !snapshot.bucket_metrics.is_empty() {
            // TRANSITION MODE: Show detailed per-bucket progress
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(3)])
                .split(inner);

            // Top: Generation transition header
            let header_line = Line::from(format!(
                "Gen: {} → {} transition │ Processing buckets...",
                generation,
                generation + 1
            ));
            f.render_widget(Paragraph::new(vec![header_line]), chunks[0]);

            // Bottom: Per-bucket progress panel (up to 8 buckets)
            let available_height = chunks[1].height as usize;
            let bucket_lines: Vec<Line> = snapshot
                .bucket_metrics
                .iter()
                .take(available_height)
                .map(|b| {
                    let (progress_bar, state_text, color) = match b.state {
                        crate::metrics::BucketState::Complete => {
                            ("████████", "Complete", Color::Green)
                        }
                        crate::metrics::BucketState::Empty => {
                            ("        ", "Empty", Color::DarkGray)
                        }
                        crate::metrics::BucketState::Draining => {
                            ("██      ", "Draining", Color::Yellow)
                        }
                        crate::metrics::BucketState::Sorting => {
                            ("████    ", "Sorting", Color::Yellow)
                        }
                        crate::metrics::BucketState::Merging => {
                            ("█████   ", "Merging", Color::Yellow)
                        }
                        crate::metrics::BucketState::AntiJoining => {
                            ("██████  ", "Anti-join", Color::Yellow)
                        }
                        crate::metrics::BucketState::Compacting => {
                            ("███████ ", "Compact", Color::Cyan)
                        }
                        crate::metrics::BucketState::Pending => {
                            ("        ", "Pending", Color::DarkGray)
                        }
                    };

                    let work_info = if b.new_work > 0 {
                        format!(" (+{} work)", format_number(b.new_work))
                    } else if matches!(b.state, crate::metrics::BucketState::Complete) {
                        String::from(" ")
                    } else {
                        String::new()
                    };

                    Line::from(vec![
                        Span::styled(format!("[{}] ", progress_bar), Style::default().fg(color)),
                        Span::styled(
                            format!("B{}: ", b.bucket_id),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(format!("{:<11}", state_text), Style::default().fg(color)),
                        Span::raw(work_info),
                    ])
                })
                .collect();

            let progress_panel = Paragraph::new(bucket_lines);
            f.render_widget(progress_panel, chunks[1]);
        } else {
            let mut lines = vec![Line::from(truncate_string(
                &format!(
                    "Gen:{} │ {} │ Work:{} │ Acc:{}",
                    generation,
                    phase_str,
                    format_number(work_len as usize),
                    format_number(seen as usize)
                ),
                inner.width as usize,
            ))];

            if snapshot.bucket_metrics.is_empty() {
                lines.push(Line::from("Land: n/a"));
                lines.push(Line::from("Health: n/a"));
            } else {
                let landing = snapshot
                    .bucket_metrics
                    .iter()
                    .map(|b| {
                        format!(
                            "B{}:{}/{}",
                            b.bucket_id,
                            format_number(b.landing_size),
                            format_bytes(b.landing_bytes as usize)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                lines.push(Line::from(truncate_string(
                    &format!("Land: {}", landing),
                    inner.width as usize,
                )));

                let health = snapshot
                    .bucket_metrics
                    .iter()
                    .map(|b| {
                        let indicator = if b.run_count > 64 {
                            "⚡"
                        } else if b.run_count > 10 {
                            "‼"
                        } else if b.run_count > 5 {
                            "!"
                        } else {
                            "✓"
                        };
                        format!("B{}[{}]{}", b.bucket_id, b.run_count, indicator)
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                lines.push(Line::from(truncate_string(
                    &format!("Health: {}", health),
                    inner.width as usize,
                )));
            }

            let generation_lines = snapshot
                .generation_stats
                .iter()
                .rev()
                .take(3)
                .rev()
                .map(|stat| {
                    Line::from(truncate_string(
                        &format_generation_stat(stat),
                        inner.width as usize,
                    ))
                })
                .collect::<Vec<_>>();
            if generation_lines.is_empty() {
                lines.push(Line::from("G: n/a"));
            } else {
                lines.extend(generation_lines);
            }

            f.render_widget(Paragraph::new(lines), inner);
        }
    }

    #[allow(dead_code)]
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

        // If no ortho data, show fallback text
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
        dims: &[Dim],
        payload: &[Option<PayloadVal>],
        vocab: &[String],
        max_width: usize,
        max_height: usize,
    ) -> Vec<String> {
        if dims.len() < 2 {
            return vec!["Invalid dimensions".to_string()];
        }

        let rows = dim_to_usize(dims[dims.len() - 2]);
        let cols = dim_to_usize(dims[dims.len() - 1]);
        let higher_dims = &dims[..dims.len() - 2];

        // Use spatial module to get proper coordinate mapping
        let location_to_index = spatial::get_location_to_index(dims);

        // Calculate max token width
        let max_token_width = payload
            .iter()
            .filter_map(|&opt| opt)
            .filter_map(|idx| vocab.get(payload_to_usize(idx)))
            .map(|s| s.len())
            .max()
            .unwrap_or(1)
            .max(4)
            .min(10); // Cap at 10 to avoid overflow

        let format_cell = |token_id: Option<PayloadVal>| -> String {
            token_id
                .and_then(|id| vocab.get(payload_to_usize(id)))
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
        fn generate_coords(dims: &[Dim], current: Vec<usize>, all: &mut Vec<Vec<usize>>) {
            if current.len() == dims.len() {
                all.push(current);
                return;
            }
            let dim_idx = current.len();
            for i in 0..dim_to_usize(dims[dim_idx]) {
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

fn format_generation_stat(stat: &crate::metrics::GenerationStat) -> String {
    format!(
        "G{} P{:.1}s T{:.1}s A{} W{}",
        stat.generation,
        stat.processing_secs,
        stat.transition_secs,
        format_number(stat.accepted as usize),
        format_number(stat.new_work as usize)
    )
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

#[allow(dead_code)]
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

fn format_snapshot(snapshot: &MetricsSnapshot) -> String {
    let mut lines = Vec::new();
    let role_label = if snapshot.global.role.is_empty() {
        "unknown".to_string()
    } else {
        snapshot.global.role.clone()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = now.saturating_sub(snapshot.global.start_time);

    lines.push(format!(
        "FOLD TUI Snapshot @ {}s since start",
        format_elapsed(elapsed)
    ));
    lines.push(format!(
        "Role: {} | Mode: {} | Phase: {} | Generation: {}",
        role_label, snapshot.global.mode, snapshot.global.phase, snapshot.global.generation
    ));
    lines.push(format!(
        "Work: {} | Accepted: {} | Fan-in: {} | Arena cap: {}",
        format_number(snapshot.global.work_len as usize),
        format_number(snapshot.global.seen_len_accepted as usize),
        snapshot.global.fan_in,
        format_bytes(snapshot.global.compaction_arena_cap_bytes)
    ));

    let disk_line = if snapshot.global.disk_total_bytes > 0 {
        let total = snapshot.global.disk_total_bytes;
        let free = snapshot.global.disk_available_bytes;
        let used = total.saturating_sub(free);
        let pct_used = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        format!(
            "Disk: used {} ({:.0}%) | free {} of {}",
            format_bytes(used as usize),
            pct_used,
            format_bytes(free as usize),
            format_bytes(total as usize)
        )
    } else {
        "Disk: n/a".to_string()
    };
    lines.push(disk_line);

    lines.push(format!(
        "RAM: total {} ({}%) | proc {} / {}",
        format_bytes(snapshot.global.ram_bytes),
        snapshot.global.system_memory_percent,
        format_bytes(snapshot.global.process_rss_bytes),
        format_bytes(snapshot.global.process_rss_cap_bytes)
    ));
    lines.push(format!(
        "Work cache: {} / {} | Segment batch: {} / {}",
        format_bytes(snapshot.global.work_cache_bytes),
        format_bytes(snapshot.global.work_cache_cap_bytes),
        format_bytes(snapshot.global.segment_batch_bytes),
        format_bytes(snapshot.global.segment_batch_cap_bytes)
    ));

    let progress = if snapshot.operation.progress_total > 0 {
        let ratio =
            snapshot.operation.progress_current as f64 / snapshot.operation.progress_total as f64;
        format!(
            "{} / {} ({:.0}%)",
            format_number(snapshot.operation.progress_current),
            format_number(snapshot.operation.progress_total),
            (ratio * 100.0).clamp(0.0, 100.0)
        )
    } else {
        "n/a".to_string()
    };
    lines.push(format!(
        "Current: {} | Status: {} | New orthos: {} | Progress: {}",
        snapshot.operation.current_file,
        snapshot.operation.status,
        format_number(snapshot.operation.new_orthos),
        progress
    ));

    lines.push(format!(
        "Spill: created {} files {} | pending {} files {} | consumed {} files {}",
        format_number(snapshot.global.spill_created_files as usize),
        format_bytes(snapshot.global.spill_created_bytes as usize),
        format_number(snapshot.global.spill_pending_files as usize),
        format_bytes(snapshot.global.spill_pending_bytes as usize),
        format_number(snapshot.global.spill_consumed_files as usize),
        format_bytes(snapshot.global.spill_consumed_bytes as usize)
    ));
    lines.push(format!(
        "Landing Bytes: {}",
        format_bytes(snapshot.global.landing_buffer_bytes as usize)
    ));

    let offload_line = format!(
        "Offload: {} files {} | Download: {} files {} | Cache hit/miss: {}/{} | Pressure triggers: {}",
        format_number(snapshot.global.offloaded_files as usize),
        format_bytes(snapshot.global.offloaded_bytes as usize),
        format_number(snapshot.global.downloaded_files as usize),
        format_bytes(snapshot.global.downloaded_bytes as usize),
        format_number(snapshot.global.cache_hits as usize),
        format_number(snapshot.global.cache_misses as usize),
        format_number(snapshot.global.pressure_triggers as usize)
    );
    lines.push(offload_line);

    if snapshot.global.compression_uncompressed_bytes > 0 {
        let unc = snapshot.global.compression_uncompressed_bytes;
        let comp = snapshot.global.compression_compressed_bytes;
        let ratio = if comp > 0 {
            unc as f64 / comp as f64
        } else {
            0.0
        };
        let saved = unc.saturating_sub(comp);
        lines.push(format!(
            "Compression: {:.2}x (saved {})",
            ratio,
            format_bytes(saved as usize)
        ));
    }

    lines.push(String::new());
    lines.push("Logs (last 50):".to_string());
    let log_lines = snapshot.logs.iter().rev().take(50).rev();
    for log in log_lines {
        lines.push(format!(
            "- [{}] {}",
            format_timestamp(log.timestamp),
            log.message
        ));
    }

    lines.join("\n")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{BucketMetrics, BucketState, GenerationStat};
    use ratatui::{Terminal, backend::TestBackend};

    fn render_screen(metrics: &Metrics, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut tui = Tui::new(
            metrics.clone_handle(),
            Arc::new(AtomicBool::new(false)),
            None,
        );

        terminal.draw(|f| tui.render(f)).unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample_metrics(transition: bool) -> Metrics {
        let metrics = Metrics::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        metrics.update_global(|g| {
            g.mode = "Merging Archives".to_string();
            g.role = "leader".to_string();
            g.interner_version = 4;
            g.vocab_size = 550;
            g.total_chunks = 204;
            g.processed_chunks = 320;
            g.remaining_chunks = 88;
            g.distinct_jobs_count = 0;
            g.start_time = now.saturating_sub(234);
            g.process_rss_cap_bytes = 8 * 1024 * 1024 * 1024;
            g.generation = 12;
            g.phase = "Idle".to_string();
            g.work_len = 1;
            g.seen_len_accepted = 7_900;
            g.compaction_arena_cap_bytes = 384 * 1024 * 1024;
            g.work_cache_cap_bytes = 256 * 1024 * 1024;
            g.segment_batch_cap_bytes = 64 * 1024 * 1024;
            g.fan_in = 64;
            g.disk_total_bytes = 983_230_000_000;
            g.disk_available_bytes = 922_640_000_000;
            g.compression_uncompressed_bytes = 36_980_000_000;
            g.compression_compressed_bytes = 4_370_000_000;
            g.current_lineage = "(((\"a\" \"b\") \"c\") \"d\")".to_string();
        });
        metrics.update_operation(|o| {
            o.current_file = "e_chunk_0136".to_string();
            o.status = "Streaming & Remapping Smaller Archive B".to_string();
            o.status_start_time = now;
            o.progress_current = 1;
            o.progress_total = 1;
            o.text_preview = "since it ... the city".to_string();
            o.word_count = 181;
        });
        metrics.update_merge(|m| {
            m.completed_merges = 116;
            m.current_merge = "merge_6111".to_string();
            m.seed_orthos_a = 7_883;
            m.seed_orthos_b = 17_201;
            m.impacted_queued_a = 4_386;
            m.impacted_queued_b = 4_921;
            m.impacted_a = 1_204;
            m.impacted_b = 1_875;
            m.text_preview_a = "since it ... the city".to_string();
            m.text_preview_b = "and there ... the gates".to_string();
            m.word_count_a = 900;
            m.word_count_b = 1_100;
            m.compaction_kept = 12_000;
            m.compaction_pruned = 320;
            m.new_orthos_from_merge = 640;
        });
        metrics.update_largest_archive(|a| {
            a.filename = "./fold_state/input/archive_1775313843_915399592=.bin".to_string();
            a.ortho_count = 17_201;
        });
        metrics.update_optimal_ortho(|o| {
            o.volume = 4;
            o.variance_num = 0;
            o.variance_den = 1;
            o.dims = vec![3, 3];
            o.fullness = 8;
            o.capacity = 9;
            o.payload = vec![Some(0), Some(1), Some(2), Some(3)];
            o.vocab = vec![
                "it".to_string(),
                "and".to_string(),
                "was".to_string(),
                "city".to_string(),
            ];
            o.last_update_time = now.saturating_sub(12);
        });
        metrics.record_prune_sample(10, 32, 1000, 0, 0);
        metrics.record_prune_sample(11, 10, 900, 0, 0);
        metrics.record_work_len(1);
        metrics.record_work_len(4_000);
        metrics.record_work_len(1);
        metrics.reset_seen_size(0);
        metrics.record_landing_buffer_count(7_900);
        metrics.record_landing_buffer_count(12_500);
        metrics.record_landing_buffer_bytes(12_500 * 1024);
        metrics.set_generation_stats(vec![
            GenerationStat {
                generation: 10,
                processing_secs: 2.1,
                transition_secs: 0.4,
                accepted: 3_400_000,
                new_work: 1_200_000,
            },
            GenerationStat {
                generation: 11,
                processing_secs: 1.7,
                transition_secs: 0.3,
                accepted: 2_800_000,
                new_work: 900_000,
            },
            GenerationStat {
                generation: 12,
                processing_secs: 1.3,
                transition_secs: 0.2,
                accepted: 2_100_000,
                new_work: 650_000,
            },
        ]);

        let bucket_metrics = (0..8)
            .map(|bucket_id| BucketMetrics {
                bucket_id,
                run_count: bucket_id,
                landing_size: bucket_id * 10,
                landing_bytes: (bucket_id as u64) * 1024,
                history_size_estimate: 0,
                state: if transition && bucket_id == 0 {
                    BucketState::Draining
                } else {
                    BucketState::Complete
                },
                new_work: if transition && bucket_id == 0 {
                    21_700_000
                } else {
                    0
                },
            })
            .collect();
        metrics.update_bucket_metrics(bucket_metrics);
        metrics.add_log("Merge stage: stage=setup elapsed_ms=127".to_string());
        metrics.add_log("Loaded 7883 orthos from larger archive A".to_string());
        metrics
    }

    #[test]
    fn dense_default_screen_combines_merge_panels_and_keeps_both_previews() {
        let metrics = sample_metrics(false);
        let screen = render_screen(&metrics, 150, 43);

        assert!(screen.contains("Current Merge"));
        assert!(screen.contains("A: since it ... the city"));
        assert!(screen.contains("B: and there ... the gates"));
        assert!(!screen.contains("Text Preview"));
        assert!(!screen.contains("Merge Progress"));
        assert!(!screen.contains("Largest Archive"));
        assert!(screen.contains("Provenance Summary"));
        assert!(!screen.contains("Provenance Tree"));
        assert!(screen.contains("G10 P2.1s T0.4s A3.4M W1.2M"));
        assert!(screen.contains("Health:"));
    }

    #[test]
    fn transition_mode_still_shows_bucket_progress_rows() {
        let metrics = sample_metrics(true);
        let screen = render_screen(&metrics, 150, 43);

        assert!(screen.contains("Gen: 12 → 13 transition"));
        assert!(screen.contains("B0: Draining"));
        assert!(screen.contains("(+21.7M work)"));
    }

    #[test]
    fn small_screen_still_keeps_a_and_b_preview_lines() {
        let metrics = sample_metrics(false);
        let screen = render_screen(&metrics, 110, 34);

        assert!(screen.contains("A: since"));
        assert!(screen.contains("B: and"));
    }
}
