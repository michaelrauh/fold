use crate::metrics::{Metrics, MetricsSnapshot};
use crate::spatial;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Gauge, List, ListItem, Paragraph, Sparkline,
    },
    Frame, Terminal,
};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Tui {
    metrics: Metrics,
    should_quit: Arc<AtomicBool>,
    log_scroll: usize,
}

impl Tui {
    pub fn new(metrics: Metrics, should_quit: Arc<AtomicBool>) -> Self {
        Self {
            metrics,
            should_quit,
            log_scroll: 0,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

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
        
        let line1 = format!("FOLD Dashboard [Time: {} │ RAM: {} MB / {}%]", 
            elapsed_str,
            format_number(snapshot.global.ram_mb),
            snapshot.global.system_memory_percent
        );
        let line2 = format!("Mode: {} │ Interner: v{} │ Vocab: {}",
            mode_truncated,
            snapshot.global.interner_version,
            format_number(snapshot.global.vocab_size)
        );
        let line3 = format!("Chunks: {} │ Processed: {} │ Remaining: {}",
            snapshot.global.total_chunks,
            snapshot.global.processed_chunks,
            snapshot.global.remaining_chunks
        );
        let line4 = format!("QBuf: {} │ Bloom: {} │ Shards: {}/{} in mem",
            format_number(snapshot.global.queue_buffer_size),
            format_number(snapshot.global.bloom_capacity),
            format_number(snapshot.global.max_shards_in_memory),
            format_number(snapshot.global.num_shards)
        );
        
        let header_lines = vec![
            Line::from(truncate_string(&line1, max_width)),
            Line::from(truncate_string(&line2, max_width)),
            Line::from(truncate_string(&line3, max_width)),
            Line::from(truncate_string(&line4, max_width)),
        ];

        let header = Paragraph::new(header_lines)
            .block(Block::default().borders(Borders::ALL));
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
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Min(5),
            ])
            .split(area);

        self.render_current_operation(f, left_chunks[0], snapshot);
        self.render_merge_progress(f, left_chunks[1], snapshot);
        self.render_optimal_ortho(f, left_chunks[2], snapshot);
        self.render_largest_archive(f, left_chunks[3], snapshot);
        self.render_provenance_tree(f, left_chunks[4], snapshot);
    }

    fn render_current_operation(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let progress_ratio = if snapshot.operation.progress_total > 0 {
            snapshot.operation.progress_current as f64 / snapshot.operation.progress_total as f64
        } else {
            0.0
        };

        let max_width = area.width.saturating_sub(2) as usize;
        let label_width = 12; // "Processing: "
        let available = max_width.saturating_sub(label_width);

        let lines = vec![
            Line::from(vec![
                Span::styled("Processing: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate_string(&snapshot.operation.current_file, available)),
            ]),
            Line::from(vec![
                Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
                Span::styled(truncate_string(&snapshot.operation.status, available.saturating_sub(8)), Style::default().fg(Color::Cyan)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw(format!(
                    "{} / {}",
                    format_number(snapshot.operation.progress_current),
                    format_number(snapshot.operation.progress_total)
                )),
            ]),
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
            .ratio(progress_ratio);
        f.render_widget(gauge, gauge_area);
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
                Span::raw(truncate_string(&snapshot.merge.current_merge, max_width.saturating_sub(9))),
            ]),
            Line::from(vec![
                Span::styled("Seeds: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("A:{} B:{}", 
                    format_number(snapshot.merge.seed_orthos_a),
                    format_number(snapshot.merge.seed_orthos_b)
                )),
            ]),
            Line::from(vec![
                Span::styled("Queued: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("A:{} B:{}", 
                    format_number(snapshot.merge.impacted_queued_a),
                    format_number(snapshot.merge.impacted_queued_b)
                )),
            ]),
            Line::from(vec![
                Span::styled("Vocab Δ: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("A:{} B:{}", 
                    format_number(snapshot.merge.impacted_a),
                    format_number(snapshot.merge.impacted_b)
                )),
            ]),
            Line::from(vec![
                Span::styled("New: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format_number(snapshot.merge.new_orthos_from_merge), Style::default().fg(Color::Green)),
            ]),
        ];

        let block = Block::default().borders(Borders::ALL).title("Merge Progress");
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
                Span::raw(format!("{}/{} ({}%)", opt.fullness, opt.capacity, fullness_pct)),
            ]),
        ];

        let block = Block::default().borders(Borders::ALL).title("Optimal Ortho");
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
                Span::raw(truncate_string(&snapshot.largest_archive.filename, max_width.saturating_sub(6))),
            ]),
            Line::from(vec![
                Span::styled("Orthos: ", Style::default().fg(Color::DarkGray)),
                Span::raw(ortho_display),
            ]),
        ];

        let block = Block::default().borders(Borders::ALL).title("Largest Archive");
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

        let block = Block::default().borders(Borders::ALL).title("Provenance Tree");
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
                Constraint::Length(6),
                Constraint::Min(10),
            ])
            .split(area);

        self.render_queue_depth_chart(f, right_chunks[0], snapshot);
        self.render_seen_size_chart(f, right_chunks[1], snapshot);
        self.render_results_chart(f, right_chunks[2], snapshot);
        self.render_optimal_ortho_display(f, right_chunks[3], snapshot);
    }

    fn render_queue_depth_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let data: Vec<u64> = snapshot
            .queue_depth_samples
            .iter()
            .map(|s| s.value as u64)
            .collect();

        let (current, peak, rate) = if data.len() >= 2 {
            let current = *data.last().unwrap_or(&0);
            let peak = *data.iter().max().unwrap_or(&0);
            let prev = data[data.len().saturating_sub(10).max(0)];
            let rate = (current as i64 - prev as i64) / 10;
            (current, peak, rate)
        } else if !data.is_empty() {
            let current = *data.last().unwrap();
            (current, current, 0)
        } else {
            (0, 0, 0)
        };

        let rate_sign = if rate >= 0 { "+" } else { "" };
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
        let data: Vec<u64> = snapshot
            .seen_size_samples
            .iter()
            .map(|s| s.value as u64)
            .collect();

        let (current, peak, rate) = if data.len() >= 2 {
            let current = *data.last().unwrap_or(&0);
            let peak = *data.iter().max().unwrap_or(&0);
            let prev = data[data.len().saturating_sub(10).max(0)];
            let rate = (current.saturating_sub(prev)) / 10;
            (current, peak, rate)
        } else if !data.is_empty() {
            let current = *data.last().unwrap();
            (current, current, 0)
        } else {
            (0, 0, 0)
        };

        let title = format!(
            "Seen │ Cur:{} Pk:{} Δ+{}/s",
            format_number(current as usize),
            format_number(peak as usize),
            format_number(rate as usize)
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

    fn render_results_chart(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let results_data: Vec<u64> = snapshot
            .results_count_samples
            .iter()
            .map(|s| s.value as u64)
            .collect();

        let (current_results, peak_results, rate) = if results_data.len() >= 2 {
            let current = *results_data.last().unwrap_or(&0);
            let peak = *results_data.iter().max().unwrap_or(&0);
            let prev = results_data[results_data.len().saturating_sub(10).max(0)];
            let rate = (current as i64 - prev as i64) / 10;
            (current, peak, rate)
        } else if !results_data.is_empty() {
            let current = *results_data.last().unwrap();
            (current, current, 0)
        } else {
            (0, 0, 0)
        };

        let max_val = results_data
            .iter()
            .max()
            .copied()
            .unwrap_or(1)
            .max(1);

        let normalized_results: Vec<u64> = results_data
            .iter()
            .map(|&v| (v * 100) / max_val)
            .collect();

        let rate_sign = if rate >= 0 { "+" } else { "" };
        let title = format!(
            "Results │ Cur:{} Pk:{} Δ{}{}/s",
            format_number(current_results as usize),
            format_number(peak_results as usize),
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
            .data(&normalized_results)
            .style(Style::default().fg(Color::Cyan));

        f.render_widget(sparkline, area);
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
                    max_width
                )))
            })
            .collect();

        let title = truncate_string("Logs (↑/↓ scroll, q quit)", max_width);
        let list = List::new(logs).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title),
        );

        f.render_widget(list, area);
    }

    fn render_optimal_ortho_display(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let opt = &snapshot.optimal_ortho;
        let max_width = area.width.saturating_sub(2) as usize;
        
        // If no ortho data, show placeholder
        if opt.dims.is_empty() || opt.payload.is_empty() {
            let lines = vec![Line::from("No optimal ortho yet")];
            let block = Block::default().borders(Borders::ALL).title("Optimal Ortho Display");
            let inner = block.inner(area);
            f.render_widget(block, area);
            let paragraph = Paragraph::new(lines);
            f.render_widget(paragraph, inner);
            return;
        }
        
        // Format the ortho display
        let display_lines = self.format_ortho_display(&opt.dims, &opt.payload, &opt.vocab, max_width);
        
        let lines: Vec<Line> = display_lines
            .into_iter()
            .map(|s| Line::from(s))
            .collect();

        let block = Block::default().borders(Borders::ALL).title("Optimal Ortho Display");
        let inner = block.inner(area);
        f.render_widget(block, area);
        
        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }
    
    fn format_ortho_display(&self, dims: &[usize], payload: &[Option<usize>], vocab: &[String], max_width: usize) -> Vec<String> {
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
                .map(|s| format!("{:>width$}", truncate_string(s, max_token_width), width = max_token_width))
                .unwrap_or_else(|| format!("{:>width$}", "·", width = max_token_width))
        };
        
        let format_2d_slice = |prefix: &[usize]| -> Vec<String> {
            (0..rows)
                .map(|row| {
                    let row_str = (0..cols)
                        .map(|col| {
                            let coords: Vec<usize> = prefix.iter().copied().chain([row, col]).collect();
                            location_to_index.get(&coords)
                                .and_then(|&idx| payload.get(idx))
                                .and_then(|&opt| opt)
                                .map(|token_id| format_cell(Some(token_id)))
                                .unwrap_or_else(|| format_cell(None))
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    truncate_string(&row_str, max_width)
                })
                .collect()
        };
        
        if higher_dims.is_empty() {
            return format_2d_slice(&[]);
        }
        
        // For higher dimensions, show first tile only (to fit in space)
        let mut result = Vec::new();
        result.push("[dim0=0, ...]".to_string());
        result.extend(format_2d_slice(&vec![0; higher_dims.len()]));
        result
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
        let content = &trimmed[1..trimmed.len()-1];
        return TreeNode::Leaf(content.to_string());
    }
    
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        let inner = &trimmed[1..trimmed.len()-1];
        
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
                return (&s[..i], &s[i+1..]);
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
    let mut lines = vec![
        format!("Tree: {} nodes, {} levels", node_count, depth),
    ];
    
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
        
        let sparkline: String = sparkline_data.iter().map(|&count| {
            if count == 0 {
                ' '
            } else {
                let index = ((count as f64 / max_count as f64) * (sparkline_chars.len() - 1) as f64).round() as usize;
                sparkline_chars[index.min(sparkline_chars.len() - 1)]
            }
        }).collect();
        
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
