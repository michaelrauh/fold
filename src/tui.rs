use crate::metrics::{Metrics, MetricsSnapshot};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TerminalGuard;

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
    vertical_scroll: usize,
    snapshot_path: Option<PathBuf>,
    last_snapshot_write: Instant,
    prev_touched_by_depth: Vec<u64>,
    level_activity_heat: Vec<f64>,
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
            vertical_scroll: 0,
            snapshot_path,
            last_snapshot_write: Instant::now(),
            prev_touched_by_depth: Vec::new(),
            level_activity_heat: Vec::new(),
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let _guard = TerminalGuard;
        self.run_loop(&mut terminal)
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
                            self.vertical_scroll = self.vertical_scroll.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            self.vertical_scroll = self.vertical_scroll.saturating_add(1);
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
        if self.last_snapshot_write.elapsed() < Duration::from_secs(3) {
            return Ok(());
        }
        std::fs::write(path, format_snapshot(&self.metrics.snapshot()))?;
        self.last_snapshot_write = Instant::now();
        Ok(())
    }

    fn render(&mut self, f: &mut Frame) {
        let snapshot = self.metrics.snapshot();
        self.update_level_activity(&snapshot);
        self.clamp_scroll(f.area(), &snapshot);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(f.area());
        self.render_left_column(f, columns[0], &snapshot);
        self.render_search(f, columns[1], &snapshot);
    }

    fn update_level_activity(&mut self, snapshot: &MetricsSnapshot) {
        let level_len = snapshot
            .global
            .seen_by_depth
            .len()
            .max(snapshot.global.descended_by_depth.len())
            .max(snapshot.global.pruned_by_depth.len());
        if self.prev_touched_by_depth.len() < level_len {
            self.prev_touched_by_depth.resize(level_len, 0);
        }
        if self.level_activity_heat.len() < level_len {
            self.level_activity_heat.resize(level_len, 0.0);
        }

        for depth in 0..level_len {
            let touched_now = snapshot
                .global
                .descended_by_depth
                .get(depth)
                .copied()
                .unwrap_or(0)
                .saturating_add(
                    snapshot
                        .global
                        .pruned_by_depth
                        .get(depth)
                        .copied()
                        .unwrap_or(0),
                );
            let touched_prev = self.prev_touched_by_depth[depth];
            let delta = touched_now.saturating_sub(touched_prev);
            let delta_intensity = if delta > 0 { 1.0 } else { 0.0 };

            self.level_activity_heat[depth] =
                (self.level_activity_heat[depth] * 0.86).max(delta_intensity);
            self.prev_touched_by_depth[depth] = touched_now;
        }
    }

    fn render_run_lines(&self, snapshot: &MetricsSnapshot) -> Vec<Line<'static>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let elapsed = now.saturating_sub(snapshot.global.start_time);
        vec![
            Line::from(format!(
                "DFS/BnB [Time: {}] [Phase: {}]",
                format_elapsed(elapsed),
                snapshot.global.phase
            )),
            Line::from(format!("Input: {}", snapshot.global.input_path)),
            Line::from(format!(
                "Expanded: {}  Pruned: {}  CPruned: {}",
                format_count(snapshot.global.nodes_expanded),
                format_count(snapshot.global.nodes_pruned),
                format_count(snapshot.global.completions_pruned)
            )),
            Line::from(format!(
                "Rates: {} n/s  {} p/s  {} accepted/s  {} cp/s",
                format_rate(snapshot.global.nodes_per_sec),
                format_rate(snapshot.global.prunes_per_sec),
                format_rate(accepted_rate(snapshot)),
                format_rate(snapshot.global.completion_prunes_per_sec)
            )),
            Line::from(format!(
                "Depth: {}/{}  Current bound: vol={} full={}",
                snapshot.global.current_depth,
                snapshot.global.max_depth,
                snapshot.global.current_bound.volume,
                snapshot.global.current_bound.fullness
            )),
            Line::from(format!(
                "Checkpoint: {}",
                format_checkpoint(
                    &snapshot.global.checkpoint_status,
                    snapshot.global.checkpoint_time,
                    now,
                )
            )),
        ]
    }

    fn render_search(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let widget = Paragraph::new(self.search_lines(area, snapshot))
            .scroll((self.vertical_scroll as u16, 0))
            .block(Block::default().borders(Borders::ALL).title("Search"));
        f.render_widget(widget, area);
    }

    fn search_lines(&self, area: Rect, snapshot: &MetricsSnapshot) -> Vec<Line<'static>> {
        if snapshot.global.parallel.enabled {
            return self.parallel_search_lines(area, snapshot);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let best_age = if snapshot.global.last_improvement_unix == 0 {
            "n/a".to_string()
        } else {
            format_elapsed(now.saturating_sub(snapshot.global.last_improvement_unix))
        };
        let bound_gap = match snapshot.global.frontier_max_bound {
            Some(bound) => format!(
                "Bound gap: frontier bound vol={} vs best vol={}",
                bound.volume, snapshot.global.incumbent_score.volume
            ),
            None => "Bound gap: none (frontier exhausted)".to_string(),
        };
        let lines = vec![
            Line::from(format!(
                "Frontier: {} open siblings",
                format_count(snapshot.global.open_siblings_total)
            )),
            Line::from(bound_gap),
            Line::from(format!(
                "Best age: {} at depth {}",
                best_age, snapshot.global.last_improvement_depth
            )),
            Line::from(format!(
                "Path:   {}",
                path_progress_chart(
                    &snapshot.global.path_progress_by_depth,
                    chart_width(area, "Path:   ")
                )
            )),
        ];
        let mut all_lines = lines;
        all_lines.push(Line::from("Touched/Seen by level"));
        all_lines.extend(level_progress_lines(
            &snapshot.global.seen_by_depth,
            &snapshot.global.descended_by_depth,
            &snapshot.global.pruned_by_depth,
            &self.level_activity_heat,
            search_level_count(snapshot),
            area,
        ));
        all_lines
    }

    fn parallel_search_lines(&self, area: Rect, snapshot: &MetricsSnapshot) -> Vec<Line<'static>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let best_age = if snapshot.global.last_improvement_unix == 0 {
            "n/a".to_string()
        } else {
            format_elapsed(now.saturating_sub(snapshot.global.last_improvement_unix))
        };
        let parallel = &snapshot.global.parallel;
        let frontier_max = snapshot
            .global
            .frontier_max_bound
            .map(|bound| bound.volume.to_string())
            .unwrap_or_else(|| "none".to_string());
        let mut lines = vec![
            Line::from(format!(
                "Workers: {}/{}  Queue: {} pending  {} running  {} done",
                parallel.workers_active,
                parallel.workers_total,
                format_count(parallel.shards_pending as u64),
                format_count(parallel.shards_running as u64),
                format_count(parallel.shards_done as u64)
            )),
            Line::from(format!(
                "Best actual vol={}  Effective prune vol={}  Floor vol={}  Frontier max vol={}",
                snapshot.global.incumbent_score.volume,
                snapshot.global.effective_prune_score.volume,
                snapshot.global.score_floor.volume,
                frontier_max
            )),
            Line::from(format!(
                "Rates: {} n/s  {} p/s  {} accepted/s  {} cp/s",
                format_rate(snapshot.global.nodes_per_sec),
                format_rate(snapshot.global.prunes_per_sec),
                format_rate(accepted_rate(snapshot)),
                format_rate(snapshot.global.completion_prunes_per_sec)
            )),
            Line::from(format!(
                "Balance min/avg/max={}/{}/{}",
                format_rate(parallel.worker_rate_min),
                format_rate(parallel.worker_rate_avg),
                format_rate(parallel.worker_rate_max)
            )),
            Line::from(format!(
                "Best age: {} at depth {}",
                best_age, snapshot.global.last_improvement_depth
            )),
        ];
        lines.extend(worker_health_lines(snapshot, area));
        lines.push(Line::from(""));
        lines.push(Line::from("Touched/Seen by level"));
        lines.extend(level_progress_lines(
            &snapshot.global.seen_by_depth,
            &snapshot.global.descended_by_depth,
            &snapshot.global.pruned_by_depth,
            &self.level_activity_heat,
            search_level_count(snapshot),
            area,
        ));
        lines
    }

    fn render_best_lines(&self, snapshot: &MetricsSnapshot) -> Vec<Line<'static>> {
        let mut lines = if snapshot.global.parallel.enabled {
            vec![
                Line::from(format!(
                    "Best actual: vol={} var={}/{} full={}",
                    snapshot.global.incumbent_score.volume,
                    snapshot.global.incumbent_score.variance_num,
                    snapshot.global.incumbent_score.variance_den,
                    snapshot.global.incumbent_score.fullness
                )),
                Line::from(format!(
                    "Effective prune: vol={} full={}",
                    snapshot.global.effective_prune_score.volume,
                    snapshot.global.effective_prune_score.fullness
                )),
                Line::from(format!("Floor: vol={}", snapshot.global.score_floor.volume)),
                Line::from(format!(
                    "Best dims: {:?}  capacity={}",
                    snapshot.global.incumbent_dims, snapshot.global.incumbent_capacity
                )),
                Line::from(""),
            ]
        } else {
            vec![
                Line::from(format!(
                    "Best score: vol={} var={}/{} full={}",
                    snapshot.global.incumbent_score.volume,
                    snapshot.global.incumbent_score.variance_num,
                    snapshot.global.incumbent_score.variance_den,
                    snapshot.global.incumbent_score.fullness
                )),
                Line::from(format!(
                    "Best dims: {:?}  capacity={}",
                    snapshot.global.incumbent_dims, snapshot.global.incumbent_capacity
                )),
                Line::from(""),
            ]
        };
        lines.extend(multiline_lines(&snapshot.global.incumbent_display));
        lines
    }

    fn render_left_column(&self, f: &mut Frame, area: Rect, snapshot: &MetricsSnapshot) {
        let widget = Paragraph::new(self.left_column_lines(snapshot))
            .wrap(Wrap { trim: false })
            .scroll((self.vertical_scroll as u16, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Run / Incumbent / Logs"),
            );
        f.render_widget(widget, area);
    }

    fn left_column_lines(&self, snapshot: &MetricsSnapshot) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(section_title("Run"));
        lines.extend(self.render_run_lines(snapshot));
        lines.push(Line::from(""));
        lines.push(section_title("Incumbent"));
        lines.extend(self.render_best_lines(snapshot));
        lines.push(Line::from(""));
        lines.push(section_title("Logs"));
        lines.extend(snapshot.logs.iter().cloned().map(Line::from));
        lines
    }

    fn clamp_scroll(&mut self, area: Rect, snapshot: &MetricsSnapshot) {
        let viewport_height = area.height.saturating_sub(2) as usize;
        if viewport_height == 0 {
            self.vertical_scroll = 0;
            return;
        }
        let left_lines = self.left_column_lines(snapshot).len();
        let right_lines = self.search_lines(area, snapshot).len();
        let content_height = left_lines.max(right_lines);
        let max_scroll = content_height.saturating_sub(viewport_height);
        self.vertical_scroll = self.vertical_scroll.min(max_scroll);
    }
}

pub fn format_snapshot(snapshot: &MetricsSnapshot) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let elapsed = now.saturating_sub(snapshot.global.start_time);
    let mut out = String::new();
    out.push_str(&format!(
        "DFS/BnB | time={} | phase={}\n",
        format_elapsed(elapsed),
        snapshot.global.phase
    ));
    out.push_str(&format!("input={}\n", snapshot.global.input_path));
    out.push_str(&format!(
        "expanded={} pruned={} completion_pruned={}\n",
        snapshot.global.nodes_expanded,
        snapshot.global.nodes_pruned,
        snapshot.global.completions_pruned
    ));
    out.push_str(&format!(
        "rates=(nodes_per_sec={:.1},prunes_per_sec={:.1},accepted_per_sec={:.1},completion_prunes_per_sec={:.1})\n",
        snapshot.global.nodes_per_sec,
        snapshot.global.prunes_per_sec,
        accepted_rate(snapshot),
        snapshot.global.completion_prunes_per_sec
    ));
    out.push_str(&format!(
        "depth={}/{} current_bound=(vol={},full={}) frontier_open={}\n",
        snapshot.global.current_depth,
        snapshot.global.max_depth,
        snapshot.global.current_bound.volume,
        snapshot.global.current_bound.fullness,
        snapshot.global.open_siblings_total
    ));
    out.push_str(&format!(
        "best_score=(vol={},var={}/{},full={}) dims={:?} capacity={}\n",
        snapshot.global.incumbent_score.volume,
        snapshot.global.incumbent_score.variance_num,
        snapshot.global.incumbent_score.variance_den,
        snapshot.global.incumbent_score.fullness,
        snapshot.global.incumbent_dims,
        snapshot.global.incumbent_capacity
    ));
    out.push_str(&format!(
        "checkpoint={}\n",
        format_checkpoint(
            &snapshot.global.checkpoint_status,
            snapshot.global.checkpoint_time,
            now,
        )
    ));
    out.push_str(&format!(
        "frontier_max_bound={}\n",
        snapshot
            .global
            .frontier_max_bound
            .map(|bound| format!(
                "(vol={},var={}/{},full={})",
                bound.volume, bound.variance_num, bound.variance_den, bound.fullness
            ))
            .unwrap_or_else(|| "none".to_string())
    ));
    out.push_str(&format!(
        "last_improvement={} depth={}\n",
        snapshot.global.last_improvement_unix, snapshot.global.last_improvement_depth
    ));
    out.push_str(&format!(
        "path={}\n",
        path_progress_chart(&snapshot.global.path_progress_by_depth, 64)
    ));
    out.push_str(&format!(
        "open={}\n",
        count_chart(&snapshot.global.open_siblings_by_depth, 64)
    ));
    out.push_str(&format!(
        "pruned={}\n\n",
        count_chart(&snapshot.global.pruned_by_depth, 64)
    ));
    out.push_str(&format!(
        "seen_by_depth={:?}\n\n",
        snapshot.global.seen_by_depth
    ));
    out.push_str(&format!(
        "tree={}\n\n",
        tree_occupancy_chart(
            &snapshot.global.descended_by_depth,
            &snapshot.global.pruned_by_depth,
            &snapshot.global.open_siblings_by_depth,
            &snapshot.global.path_progress_by_depth,
            64
        )
    ));
    out.push_str(&snapshot.global.incumbent_display);
    out.push_str("\n\nLogs:\n");
    for line in &snapshot.logs {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn format_elapsed(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_checkpoint(status: &str, checkpoint_time: u64, now: u64) -> String {
    if checkpoint_time == 0 {
        format!("{status} @ n/a")
    } else {
        format!(
            "{status} @ {checkpoint_time} age {}",
            format_elapsed(now.saturating_sub(checkpoint_time))
        )
    }
}

fn multiline_lines(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![Line::from("")];
    }
    text.lines()
        .map(|line| Line::from(line.to_string()))
        .collect()
}

fn search_level_count(snapshot: &MetricsSnapshot) -> usize {
    let vector_len = snapshot
        .global
        .seen_by_depth
        .len()
        .max(snapshot.global.descended_by_depth.len())
        .max(snapshot.global.pruned_by_depth.len());
    let deepest_non_empty = (0..vector_len)
        .rfind(|&idx| {
            snapshot.global.seen_by_depth.get(idx).copied().unwrap_or(0) > 0
                || snapshot
                    .global
                    .descended_by_depth
                    .get(idx)
                    .copied()
                    .unwrap_or(0)
                    > 0
                || snapshot
                    .global
                    .pruned_by_depth
                    .get(idx)
                    .copied()
                    .unwrap_or(0)
                    > 0
        })
        .map(|idx| idx + 1)
        .unwrap_or(0);
    deepest_non_empty
}

fn section_title(title: &str) -> Line<'static> {
    Line::from(format!("[{title}]"))
}

fn chart_width(area: Rect, label: &str) -> usize {
    area.width
        .saturating_sub(2)
        .saturating_sub(label.len() as u16) as usize
}

fn level_progress_lines(
    seen_by_depth: &[u64],
    descended_by_depth: &[u64],
    pruned_by_depth: &[u64],
    activity_heat: &[f64],
    levels: usize,
    area: Rect,
) -> Vec<Line<'static>> {
    (0..levels)
        .map(|idx| {
            let seen = seen_by_depth.get(idx).copied().unwrap_or(0);
            let touched = descended_by_depth
                .get(idx)
                .copied()
                .unwrap_or(0)
                .saturating_add(pruned_by_depth.get(idx).copied().unwrap_or(0));
            let line_width = area.width as usize;
            let percent = if seen == 0 {
                "  n/a".to_string()
            } else {
                format!("{:>5.1}%", 100.0 * touched as f64 / seen as f64)
            };
            let counts = format!("{}/{}", format_count(touched), format_count(seen));
            let prefix = format!("L{:02} {percent} {counts:>11} ", idx + 1);
            let bar_width = line_width
                .saturating_sub(2)
                .saturating_sub(prefix.chars().count())
                .max(8);
            let bar = percent_bar(touched, seen, bar_width);
            let heat = activity_heat.get(idx).copied().unwrap_or(0.0);
            let style = activity_style(heat, seen == 0);
            Line::from(vec![Span::raw(prefix), Span::styled(bar, style)])
        })
        .collect()
}

fn percent_bar(touched: u64, seen: u64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if seen == 0 {
        return "·".repeat(width);
    }
    let ratio = (touched as f64 / seen as f64).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let mut out = String::with_capacity(width);
    out.push_str(&"█".repeat(filled));
    out.push_str(&"·".repeat(width.saturating_sub(filled)));
    out
}

fn activity_style(heat: f64, unseen: bool) -> Style {
    if unseen {
        return Style::default().fg(Color::DarkGray);
    }
    if heat >= 0.70 {
        Style::default().fg(Color::Red)
    } else if heat >= 0.35 {
        Style::default().fg(Color::Yellow)
    } else if heat >= 0.10 {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn format_count(value: u64) -> String {
    const UNITS: [&str; 5] = ["", "k", "M", "B", "T"];
    let mut scaled = value as f64;
    let mut unit_idx = 0usize;
    while scaled >= 1000.0 && unit_idx + 1 < UNITS.len() {
        scaled /= 1000.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        value.to_string()
    } else if scaled >= 100.0 {
        format!("{scaled:.0}{}", UNITS[unit_idx])
    } else if scaled >= 10.0 {
        format!("{scaled:.1}{}", UNITS[unit_idx])
    } else {
        format!("{scaled:.2}{}", UNITS[unit_idx])
    }
}

fn format_rate(value: f64) -> String {
    if value <= 0.0 {
        "0".to_string()
    } else if value >= 1.0 {
        format_count(value.round() as u64)
    } else {
        format!("{value:.2}")
    }
}

fn accepted_rate(snapshot: &MetricsSnapshot) -> f64 {
    (snapshot.global.nodes_per_sec - snapshot.global.prunes_per_sec).max(0.0)
}

fn path_progress_chart(values: &[(usize, usize)], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let bins = depth_bins(values.len(), width);
    bins.into_iter()
        .map(|(start, end)| {
            let mut ratio_sum = 0.0;
            let mut known = 0usize;
            for &(next_idx, branch_count) in &values[start..end] {
                if branch_count == 0 {
                    continue;
                }
                ratio_sum += next_idx as f64 / branch_count as f64;
                known += 1;
            }
            if known == 0 {
                '·'
            } else {
                progress_glyph(ratio_sum / known as f64)
            }
        })
        .collect()
}

fn worker_health_lines(snapshot: &MetricsSnapshot, area: Rect) -> Vec<Line<'static>> {
    let workers = &snapshot.global.parallel.worker_summaries;
    if workers.is_empty() {
        return vec![Line::from("Workers: idle")];
    }
    if workers.len() > 4 {
        let slowest = snapshot
            .global
            .parallel
            .slowest_worker
            .map(|id| format!(" slowest=W{id}"))
            .unwrap_or_default();
        return vec![Line::from(format!(
            "Workers: {}/{} active min/avg/max={}/{}/{}{}",
            snapshot.global.parallel.workers_active,
            snapshot.global.parallel.workers_total,
            format_rate(snapshot.global.parallel.worker_rate_min),
            format_rate(snapshot.global.parallel.worker_rate_avg),
            format_rate(snapshot.global.parallel.worker_rate_max),
            slowest
        ))];
    }

    let width = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    let mut current = String::from("Workers:");
    for worker in workers {
        let bound = worker
            .current_bound
            .map(|score| format!(" vol={}", format_count(score.volume as u64)))
            .unwrap_or_default();
        let shard = worker
            .shard_id
            .map(|id| format!(" s{id}"))
            .unwrap_or_default();
        let item = format!(
            " W{}{} d{} {}/s{}",
            worker.id,
            shard,
            worker.depth,
            format_rate(worker.rate),
            bound
        );
        if current.chars().count() + item.chars().count() > width && current != "Workers:" {
            lines.push(Line::from(current));
            current = String::from("        ");
        }
        current.push_str(&item);
    }
    lines.push(Line::from(current));
    lines
}

fn count_chart(values: &[u64], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let bins = depth_bins(values.len(), width);
    let mut binned = Vec::with_capacity(bins.len());
    for (start, end) in bins {
        binned.push(values[start..end].iter().copied().sum::<u64>());
    }
    let max_value = binned.iter().copied().max().unwrap_or(0);
    binned
        .into_iter()
        .map(|value| count_glyph(value, max_value))
        .collect()
}

fn tree_occupancy_chart(
    descended_by_depth: &[u64],
    pruned_by_depth: &[u64],
    open_siblings_by_depth: &[u64],
    path_progress_by_depth: &[(usize, usize)],
    width: usize,
) -> String {
    if width == 0 {
        return String::new();
    }

    let depth_len = descended_by_depth
        .len()
        .max(pruned_by_depth.len())
        .max(open_siblings_by_depth.len())
        .max(path_progress_by_depth.len());
    let bins = depth_bins(depth_len, width);

    bins.into_iter()
        .map(|(start, end)| {
            if start >= depth_len {
                return '·';
            }

            let mut visited = 0u64;
            let mut open = 0u64;
            let mut unknown = false;

            for depth in start..end {
                visited = visited
                    .saturating_add(descended_by_depth.get(depth).copied().unwrap_or(0))
                    .saturating_add(pruned_by_depth.get(depth).copied().unwrap_or(0));
                open = open.saturating_add(open_siblings_by_depth.get(depth).copied().unwrap_or(0));

                if let Some((next_idx, branch_count)) = path_progress_by_depth.get(depth) {
                    if *branch_count == 0 && *next_idx == 0 {
                        unknown = true;
                    }
                }
            }

            tree_glyph(visited, open, unknown)
        })
        .collect()
}

fn depth_bins(len: usize, width: usize) -> Vec<(usize, usize)> {
    if len == 0 {
        return vec![(0, 0)];
    }
    if len <= width {
        return (0..len).map(|idx| (idx, idx + 1)).collect();
    }
    (0..width)
        .map(|bin| {
            let start = bin * len / width;
            let end = ((bin + 1) * len / width).max(start + 1);
            (start, end.min(len))
        })
        .collect()
}

fn progress_glyph(progress: f64) -> char {
    const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let clamped = progress.clamp(0.0, 1.0);
    let idx = ((clamped * (GLYPHS.len() - 1) as f64).round() as usize).min(GLYPHS.len() - 1);
    GLYPHS[idx]
}

fn count_glyph(value: u64, max_value: u64) -> char {
    const GLYPHS: [char; 9] = ['·', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if value == 0 || max_value == 0 {
        return GLYPHS[0];
    }
    let idx = (((value as f64 / max_value as f64) * (GLYPHS.len() - 1) as f64).ceil() as usize)
        .min(GLYPHS.len() - 1);
    GLYPHS[idx]
}

fn tree_glyph(visited: u64, open: u64, unknown: bool) -> char {
    if unknown && visited == 0 && open == 0 {
        return '?';
    }
    if visited == 0 && open == 0 {
        return '·';
    }
    let total_known = visited.saturating_add(open);
    if total_known == 0 {
        return '?';
    }
    let ratio = visited as f64 / total_known as f64;
    if ratio >= 0.875 {
        '█'
    } else if ratio >= 0.625 {
        '▓'
    } else if ratio >= 0.375 {
        '▒'
    } else {
        '░'
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MetricsSnapshot, Tui, accepted_rate, activity_style, count_chart, format_checkpoint,
        format_snapshot, multiline_lines, path_progress_chart, percent_bar, search_level_count,
        tree_occupancy_chart,
    };
    use crate::metrics::GlobalMetrics;
    use crate::metrics::Metrics;
    use crate::ortho::OrthoScore;
    use ratatui::layout::Rect;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn multiline_lines_preserve_row_boundaries() {
        let lines = multiline_lines("a b c\nx y z\n· · ·");
        let rendered: Vec<String> = lines.into_iter().map(|line| line.to_string()).collect();
        assert_eq!(rendered, vec!["a b c", "x y z", "· · ·"]);
    }

    #[test]
    fn charts_downsample_deterministically() {
        assert_eq!(
            path_progress_chart(&[(0, 0), (1, 4), (4, 4), (2, 4)], 2)
                .chars()
                .count(),
            2
        );
        assert_eq!(count_chart(&[0, 1, 2, 3, 4, 5], 3).chars().count(), 3);
        assert_eq!(
            tree_occupancy_chart(&[4, 5, 6], &[1, 0, 0], &[2, 0, 1], &[(1, 3), (0, 0)], 2)
                .chars()
                .count(),
            2
        );
        assert_eq!(percent_bar(3, 4, 10).chars().count(), 10);
        assert_eq!(
            format!("{:?}", activity_style(0.8, false)),
            "Style::new().red()"
        );
    }

    #[test]
    fn checkpoint_line_includes_age() {
        assert_eq!(
            format_checkpoint("saved in 0.22s", 1_778_078_041, 1_778_081_641),
            "saved in 0.22s @ 1778078041 age 01:00:00"
        );
    }

    #[test]
    fn checkpoint_line_handles_missing_time() {
        assert_eq!(
            format_checkpoint("Not yet checkpointed", 0, 1_778_081_641),
            "Not yet checkpointed @ n/a"
        );
    }

    #[test]
    fn snapshot_format_includes_frontier_and_depth_rows() {
        let mut snapshot = MetricsSnapshot::default();
        snapshot.global = GlobalMetrics {
            phase: "Running".to_string(),
            input_path: "e.txt".to_string(),
            start_time: 1,
            nodes_expanded: 100,
            nodes_pruned: 20,
            completions_pruned: 5,
            current_depth: 3,
            max_depth: 5,
            current_bound: OrthoScore::optimistic_bound(3, 4),
            incumbent_score: OrthoScore::optimistic_bound(2, 3),
            incumbent_dims: vec![2, 3],
            incumbent_capacity: 6,
            incumbent_display: "a b ·\nc d ·".to_string(),
            checkpoint_time: 2,
            checkpoint_status: "periodic".to_string(),
            open_siblings_total: 9,
            open_siblings_by_depth: vec![4, 3, 2],
            seen_by_depth: vec![9, 6, 4],
            descended_by_depth: vec![8, 4, 1],
            pruned_by_depth: vec![1, 2, 3],
            path_progress_by_depth: vec![(1, 4), (2, 4), (0, 0)],
            frontier_max_bound: Some(OrthoScore::optimistic_bound(5, 6)),
            last_improvement_unix: 1,
            last_improvement_depth: 3,
            nodes_per_sec: 1000.0,
            prunes_per_sec: 200.0,
            completion_prunes_per_sec: 50.0,
            ..GlobalMetrics::default()
        };

        let rendered = format_snapshot(&snapshot);
        assert!(rendered.contains("frontier_open=9"));
        assert!(rendered.contains("seen_by_depth="));
        assert!(rendered.contains("path="));
        assert!(rendered.contains("open="));
        assert!(rendered.contains("pruned="));
        assert!(rendered.contains("accepted_per_sec=800.0"));
        assert!(rendered.contains("tree="));
    }

    #[test]
    fn accepted_rate_saturates_at_zero() {
        let mut snapshot = MetricsSnapshot::default();
        snapshot.global.nodes_per_sec = 100.0;
        snapshot.global.prunes_per_sec = 150.0;

        assert_eq!(accepted_rate(&snapshot), 0.0);
    }

    #[test]
    fn clamp_scroll_caps_to_tallest_column() {
        let mut snapshot = MetricsSnapshot::default();
        snapshot.global = GlobalMetrics {
            phase: "Running".to_string(),
            input_path: "e.txt".to_string(),
            start_time: 1,
            incumbent_display: (0..20)
                .map(|idx| format!("row {idx}"))
                .collect::<Vec<_>>()
                .join("\n"),
            ..GlobalMetrics::default()
        };
        snapshot.logs = (0..5).map(|idx| format!("log {idx}")).collect();

        let metrics = Metrics::new();
        let should_quit = Arc::new(AtomicBool::new(false));
        let mut tui = Tui::new(metrics, should_quit, None);
        tui.vertical_scroll = 100;
        let area = Rect::new(0, 0, 80, 10);
        tui.clamp_scroll(area, &snapshot);
        let expected = tui
            .left_column_lines(&snapshot)
            .len()
            .max(tui.search_lines(area, &snapshot).len())
            .saturating_sub(area.height.saturating_sub(2) as usize);

        assert_eq!(tui.vertical_scroll, expected);
    }

    #[test]
    fn search_lines_include_all_known_levels() {
        let mut snapshot = MetricsSnapshot::default();
        snapshot.global.seen_by_depth = vec![1; 50];
        snapshot.global.descended_by_depth = vec![0; 50];
        snapshot.global.pruned_by_depth = vec![0; 50];

        let metrics = Metrics::new();
        let should_quit = Arc::new(AtomicBool::new(false));
        let mut tui = Tui::new(metrics, should_quit, None);
        tui.update_level_activity(&snapshot);

        let rendered: Vec<String> = tui
            .search_lines(Rect::new(0, 0, 80, 20), &snapshot)
            .into_iter()
            .map(|line| line.to_string())
            .collect();

        assert!(rendered.iter().any(|line| line.starts_with("L50 ")));
    }

    #[test]
    fn search_level_count_ignores_trailing_zero_rows() {
        let mut snapshot = MetricsSnapshot::default();
        snapshot.global.current_depth = 21;
        snapshot.global.seen_by_depth = vec![10, 5, 0, 0];
        snapshot.global.descended_by_depth = vec![10, 2, 0, 0];
        snapshot.global.pruned_by_depth = vec![0, 1, 0, 0];

        assert_eq!(search_level_count(&snapshot), 2);
    }
}
