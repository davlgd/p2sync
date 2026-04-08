use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use libp2p::PeerId;
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tokio::sync::mpsc;

use p2sync_core::config::TuiConfig;
use p2sync_net::sync_engine::{SyncEvent, TransferDirection};

struct PeerInfo {
    id: PeerId,
    last_sync: Instant,
    connected: bool,
}

struct TransferInfo {
    path: PathBuf,
    direction: TransferDirection,
    current_chunk: usize,
    total_chunks: usize,
}

pub struct AppState {
    peer_id: String,
    group_id: String,
    root_path: String,
    file_count: usize,
    total_size: u64,
    listen_addrs: Vec<String>,
    peers: Vec<PeerInfo>,
    transfers: Vec<TransferInfo>,
    journal: VecDeque<String>,
    tui_config: TuiConfig,
}

impl AppState {
    pub fn new(
        peer_id: String,
        group_id: String,
        root_path: String,
        tui_config: TuiConfig,
    ) -> Self {
        let max_log = tui_config.max_log_lines;
        Self {
            peer_id,
            group_id,
            root_path,
            file_count: 0,
            total_size: 0,
            listen_addrs: Vec::new(),
            peers: Vec::new(),
            transfers: Vec::new(),
            journal: VecDeque::with_capacity(max_log),
            tui_config,
        }
    }

    fn add_log(&mut self, msg: String) {
        if self.journal.len() >= self.tui_config.max_log_lines {
            self.journal.pop_front();
        }
        let timestamp = chrono_now();
        self.journal.push_back(format!("{timestamp} {msg}"));
    }

    fn handle_event(&mut self, event: SyncEvent) {
        match event {
            SyncEvent::IndexReady {
                file_count,
                total_size,
            } => {
                self.file_count = file_count;
                self.total_size = total_size;
                self.add_log(format!(
                    "{file_count} files indexed ({} total)",
                    format_size(total_size)
                ));
            }
            SyncEvent::PeerConnected(peer_id) => {
                if !self.peers.iter().any(|p| p.id == peer_id) {
                    self.peers.push(PeerInfo {
                        id: peer_id,
                        last_sync: Instant::now(),
                        connected: true,
                    });
                } else {
                    for p in &mut self.peers {
                        if p.id == peer_id {
                            p.connected = true;
                            p.last_sync = Instant::now();
                        }
                    }
                }
            }
            SyncEvent::PeerDisconnected(peer_id) => {
                for p in &mut self.peers {
                    if p.id == peer_id {
                        p.connected = false;
                    }
                }
            }
            SyncEvent::Listening(addr) => {
                self.listen_addrs.push(addr);
            }
            SyncEvent::TransferStarted {
                path,
                direction,
                total_chunks,
            } => {
                self.transfers.push(TransferInfo {
                    path,
                    direction,
                    current_chunk: 0,
                    total_chunks,
                });
            }
            SyncEvent::ChunkTransferred {
                path, chunk_index, ..
            } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.path == path) {
                    t.current_chunk = chunk_index + 1;
                    // Update peer last_sync time
                    for p in &mut self.peers {
                        if p.connected {
                            p.last_sync = Instant::now();
                        }
                    }
                }
            }
            SyncEvent::TransferComplete { path, .. } => {
                self.transfers.retain(|t| t.path != path);
            }
            SyncEvent::Conflict {
                path,
                conflict_path,
            } => {
                self.add_log(format!(
                    "CONFLICT {} -> {}",
                    path.display(),
                    conflict_path.display()
                ));
            }
            SyncEvent::RemoteDelete { path } => {
                self.add_log(format!("synced deletion {}", path.display()));
            }
            SyncEvent::ReconciliationComplete { peer, files_synced } => {
                let short = p2sync_core::util::truncate_end(&peer.to_string(), 15);
                self.add_log(format!(
                    "sync with {short}... complete ({files_synced} files)"
                ));
            }
            SyncEvent::Log(msg) => {
                self.add_log(msg);
            }
        }
    }
}

pub async fn run_tui(
    mut state: AppState,
    mut event_rx: mpsc::Receiver<SyncEvent>,
) -> anyhow::Result<()> {
    io::stdout().execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut state, &mut event_rx).await;

    ratatui::restore();
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    result
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    state: &mut AppState,
    event_rx: &mut mpsc::Receiver<SyncEvent>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, state))?;

        // Non-blocking: check for sync events and keyboard input
        tokio::select! {
            Some(sync_event) = event_rx.recv() => {
                state.handle_event(sync_event);
            }
            _ = tokio::time::sleep(state.tui_config.refresh_interval()) => {
                // Check for keyboard input
                if event::poll(std::time::Duration::ZERO)?
                    && let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                    && (key.code == KeyCode::Char('q') || key.code == KeyCode::Esc)
                {
                    return Ok(());
                }
            }
        }
    }
}

fn render(frame: &mut ratatui::Frame, state: &AppState) {
    let [header, body, journal] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Percentage(35),
    ])
    .areas(frame.area());

    let [peers_area, transfers_area] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);

    // Header
    let peer_short = p2sync_core::util::truncate_end(&state.peer_id, 19);
    let addr_display = state
        .listen_addrs
        .first()
        .cloned()
        .unwrap_or_else(|| "waiting...".to_string());

    let header_text = vec![
        Line::from(vec![
            Span::styled(
                " p2sync ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {} ", state.root_path)),
            Span::styled(
                format!("{peer_short}..."),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![Span::raw(format!(
            " {} files | {} | group: {} | {}",
            state.file_count,
            format_size(state.total_size),
            state.group_id,
            addr_display,
        ))]),
        Line::from(Span::styled(
            " Press 'q' to quit",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let header_block = Paragraph::new(header_text).block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header_block, header);

    // Peers panel
    let peer_items: Vec<ListItem> = state
        .peers
        .iter()
        .map(|p| {
            let status = if p.connected { "+" } else { "-" };
            let color = if p.connected {
                Color::Green
            } else {
                Color::Red
            };
            let elapsed = p.last_sync.elapsed().as_secs();
            let time_str = if elapsed < 60 {
                format!("{elapsed}s ago")
            } else {
                format!("{}m ago", elapsed / 60)
            };
            let peer_short = p2sync_core::util::truncate_end(&p.id.to_string(), 15);
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {status} "), Style::default().fg(color)),
                Span::raw(format!("{peer_short}...  last sync: {time_str}")),
            ]))
        })
        .collect();

    let connected = state.peers.iter().filter(|p| p.connected).count();
    let peers_block = List::new(peer_items).block(
        Block::default()
            .title(format!(" Peers ({connected}) "))
            .borders(Borders::ALL),
    );
    frame.render_widget(peers_block, peers_area);

    // Transfers panel
    let transfer_items: Vec<ListItem> = state
        .transfers
        .iter()
        .map(|t| {
            let arrow = match t.direction {
                TransferDirection::Upload => "^",
                TransferDirection::Download => "v",
            };
            let pct = if t.total_chunks > 0 {
                (t.current_chunk * 100) / t.total_chunks
            } else {
                0
            };
            let path_display = p2sync_core::util::truncate_start(&t.path.to_string_lossy(), 30);
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {arrow} "), Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{path_display}  {}/{} chunks  [{pct}%]",
                    t.current_chunk, t.total_chunks
                )),
            ]))
        })
        .collect();

    let transfers_block = List::new(transfer_items).block(
        Block::default()
            .title(format!(" Transfers ({}) ", state.transfers.len()))
            .borders(Borders::ALL),
    );
    frame.render_widget(transfers_block, transfers_area);

    // Journal panel
    let journal_items: Vec<ListItem> = state
        .journal
        .iter()
        .rev()
        .take(journal.height as usize - 2)
        .map(|msg| {
            let color = if msg.contains("CONFLICT") {
                Color::Red
            } else if msg.contains("error") || msg.contains("failed") {
                Color::Yellow
            } else {
                Color::White
            };
            ListItem::new(Span::styled(format!(" {msg}"), Style::default().fg(color)))
        })
        .collect();

    let journal_block =
        List::new(journal_items).block(Block::default().title(" Journal ").borders(Borders::ALL));
    frame.render_widget(journal_block, journal);
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs = now % 60;
    let mins = (now / 60) % 60;
    let hours = (now / 3600) % 24;
    format!("{hours:02}:{mins:02}:{secs:02}")
}
