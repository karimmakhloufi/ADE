//! ADE - a minimalist Agentic Development Environment.
//! Layout: projects (folders of the workspace) | conversation with Claude.
//! ADE does not open or edit files itself: everything goes through the agent.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod clipboard;
mod config;
mod media;
mod projects;
mod sessions;
mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, row, scrollable, text, text_input,
    vertical_rule, Column,
};
use iced::{font, Alignment, Color, Element, Font, Length, Task, Theme};

use agent::AgentEvent;

const MUTED: Color = Color { r: 0.55, g: 0.57, b: 0.62, a: 1.0 };
const GREEN: Color = Color { r: 0.45, g: 0.80, b: 0.50, a: 1.0 };
const RED: Color = Color { r: 0.92, g: 0.42, b: 0.42, a: 1.0 };
const BLUE: Color = Color { r: 0.45, g: 0.65, b: 0.95, a: 1.0 };
const TEXT: Color = Color { r: 0.82, g: 0.83, b: 0.86, a: 1.0 };
const ITALIC: Font = Font { style: font::Style::Italic, ..Font::DEFAULT };
const BOLD: Font = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
/// The default font lacks glyphs like ↻ ◐ ● ○ ✓ ✗ and iced does not fall back
/// automatically, so symbols are rendered with a font that has them.
const SYMBOL: Font = Font::with_name("Segoe UI Symbol");
/// Media control icons (private-use code points of Segoe MDL2 Assets).
const ICONS: Font = Font::with_name("Segoe MDL2 Assets");
const ICON_PREV: &str = "\u{E892}";
const ICON_NEXT: &str = "\u{E893}";
const ICON_PLAY: &str = "\u{E768}";
const ICON_PAUSE: &str = "\u{E769}";
const MAX_DISPLAY_CHARS: usize = 4_000;

fn main() -> iced::Result {
    let icon = iced::window::icon::from_rgba(include_bytes!("../assets/ade-64.rgba").to_vec(), 64, 64).ok();
    iced::application(Ade::title, Ade::update, Ade::view)
        .theme(|_| Theme::Dark)
        .subscription(Ade::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1440.0, 880.0),
            icon,
            // Conversations are saved before the window actually closes.
            exit_on_close_request: false,
            ..Default::default()
        })
        .run_with(Ade::new)
}

#[derive(Debug, Clone)]
enum Message {
    // Projects / workspace
    SelectProject(PathBuf),
    RefreshProjects,
    /// Show the inline "new project" field.
    NewProject,
    NewProjectNameChanged(String),
    CreateProject,
    CancelNewProject,
    ChangeWorkspace,
    PickWorkspace,
    WorkspacePicked(Option<PathBuf>),
    ConfirmWorkspace,
    CancelWorkspaceSetup,
    // Chat
    InputChanged(String),
    Send,
    Stop,
    NewChat,
    /// An event from the agent working on the given project.
    Agent(PathBuf, AgentEvent),
    ToggleTool(String),
    /// Paste an image from the clipboard. `true` when triggered by the button
    /// (reports "no image"), `false` for Ctrl+V (silent: it may be text).
    PasteImage(bool),
    ImagePasted(PathBuf, bool, Result<clipboard::Attachment, String>),
    RemoveAttachment(usize),
    // API key
    KeyInputChanged(String),
    SubmitKey,
    KeyValidated(Result<(), String>),
    OpenKeysPage,
    ChangeKey,
    CancelKeySetup,
    // Now playing (Spotify in the browser, or any media session)
    MediaTick,
    MediaUpdated(Option<media::NowPlaying>),
    MediaControl(media::Command),
    /// The user closes the window: save every conversation, then exit.
    CloseRequested(iced::window::Id),
}

#[derive(Default)]
struct KeySetup {
    input: String,
    error: Option<String>,
    checking: bool,
}

struct WorkspaceSetup {
    path: Option<PathBuf>,
    error: Option<String>,
}

/// Inline form at the top of the project list.
#[derive(Default)]
struct NewProject {
    name: String,
    error: Option<String>,
}

fn new_project_input_id() -> text_input::Id {
    text_input::Id::new("new-project")
}

enum ChatItem {
    User(String, Vec<iced::widget::image::Handle>),
    Assistant(String),
    Thinking(String),
    Tool {
        id: String,
        name: String,
        summary: String,
        output: Option<String>,
        is_error: bool,
        diff: Option<String>,
        expanded: bool,
    },
    Error(String),
}

/// One conversation per project. Each one runs its own agent, so several
/// projects can work at the same time.
struct Conversation {
    chat: Vec<ChatItem>,
    history: agent::History,
    tokens: (u64, u64, u64),
    /// Draft message, kept when switching projects.
    input: String,
    /// Images pasted for the next message.
    attachments: Vec<clipboard::Attachment>,
    running: Option<iced::task::Handle>,
    /// The agent finished (or failed) while another project was displayed.
    unread: bool,
}

impl Default for Conversation {
    fn default() -> Self {
        Self {
            chat: Vec::new(),
            history: Arc::new(Mutex::new(Vec::new())),
            tokens: (0, 0, 0),
            input: String::new(),
            attachments: Vec::new(),
            running: None,
            unread: false,
        }
    }
}

impl Conversation {
    /// Restores a conversation saved by a previous session.
    fn from_saved(saved: sessions::Saved) -> Self {
        let chat = chat_from_history(&saved.history, &saved.diffs);
        let history: agent::History = Arc::new(Mutex::new(saved.history));
        // ADE may have been killed in the middle of a tool call.
        agent::repair(&history);
        Self { chat, history, tokens: saved.tokens, ..Self::default() }
    }

    fn find_tool(&mut self, id: &str) -> Option<&mut ChatItem> {
        self.chat
            .iter_mut()
            .rev()
            .find(|i| matches!(i, ChatItem::Tool { id: x, .. } if x == id))
    }
}

struct Ade {
    workspace: Option<PathBuf>,
    projects: Vec<PathBuf>,
    root: Option<PathBuf>,
    convs: HashMap<PathBuf, Conversation>,
    api_key: Option<(String, config::KeySource)>,
    key_setup: Option<KeySetup>,
    workspace_setup: Option<WorkspaceSetup>,
    new_project: Option<NewProject>,
    status: String,
    now_playing: Option<media::NowPlaying>,
}

impl Ade {
    fn new() -> (Self, Task<Message>) {
        let workspace = config::load_workspace();
        let mut app = Self {
            projects: workspace.as_deref().map(projects::list).unwrap_or_default(),
            workspace,
            root: None,
            convs: sessions::load_all()
                .into_iter()
                .map(|s| (s.root.clone(), Conversation::from_saved(s)))
                .collect(),
            api_key: config::load_api_key(),
            key_setup: None,
            workspace_setup: None,
            new_project: None,
            status: "Prêt".into(),
            now_playing: None,
        };
        // First launch: API key, then workspace folder.
        if app.api_key.is_none() {
            app.key_setup = Some(KeySetup::default());
        }
        if app.workspace.is_none() {
            app.workspace_setup = Some(WorkspaceSetup { path: projects::suggested_workspace(), error: None });
        }
        if let Some(dir) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_dir()) {
            app.select_project(&dir);
        }
        // Start maximized.
        let maximize = iced::window::get_oldest()
            .and_then(|id| iced::window::maximize(id, true));
        let media = Task::perform(media::current(), Message::MediaUpdated);
        (app, Task::batch([maximize, media]))
    }

    fn title(&self) -> String {
        match &self.root {
            Some(r) => format!("ADE - {}", projects::name(r)),
            None => "ADE".into(),
        }
    }

    fn select_project(&mut self, dir: &Path) {
        let dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.convs.entry(dir.clone()).or_default().unread = false;
        self.status = format!("Projet : {}", projects::name(&dir));
        self.root = Some(dir);
    }

    /// Conversation of the displayed project.
    fn conv(&self) -> Option<&Conversation> {
        self.convs.get(self.root.as_ref()?)
    }

    fn conv_mut(&mut self) -> Option<&mut Conversation> {
        self.convs.get_mut(self.root.as_ref()?)
    }

    fn running_count(&self) -> usize {
        self.convs.values().filter(|c| c.running.is_some()).count()
    }

    /// Writes the conversation of `project` to disk (deletes it when empty).
    fn save_conv(&mut self, project: &Path) {
        let Some(conv) = self.convs.get(project) else { return };
        let diffs: HashMap<String, String> = conv
            .chat
            .iter()
            .filter_map(|i| match i {
                ChatItem::Tool { id, diff: Some(d), .. } => Some((id.clone(), d.clone())),
                _ => None,
            })
            .collect();
        let result = {
            let history = conv.history.lock().unwrap();
            sessions::save(project, &history, conv.tokens, &diffs)
        };
        if let Err(e) = result {
            self.status = format!("Sauvegarde de la conversation impossible : {e:#}");
        }
    }

    fn refresh_projects(&mut self) {
        self.projects = self.workspace.as_deref().map(projects::list).unwrap_or_default();
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SelectProject(dir) => self.select_project(&dir),
            Message::RefreshProjects => self.refresh_projects(),
            Message::NewProject => {
                self.new_project = Some(NewProject::default());
                return text_input::focus(new_project_input_id());
            }
            Message::NewProjectNameChanged(s) => {
                if let Some(np) = &mut self.new_project {
                    np.name = s;
                    np.error = None;
                }
            }
            Message::CreateProject => {
                let (Some(np), Some(ws)) = (&mut self.new_project, &self.workspace) else { return Task::none() };
                match projects::create(ws, &np.name) {
                    Ok(dir) => {
                        self.new_project = None;
                        self.refresh_projects();
                        self.select_project(&dir);
                        self.status = format!("Projet créé : {}", projects::name(&dir));
                    }
                    Err(e) => np.error = Some(e),
                }
            }
            Message::CancelNewProject => self.new_project = None,
            Message::ChangeWorkspace => {
                self.workspace_setup = Some(WorkspaceSetup { path: self.workspace.clone(), error: None });
            }
            Message::PickWorkspace => {
                let start = self
                    .workspace_setup
                    .as_ref()
                    .and_then(|s| s.path.clone())
                    .or_else(|| self.workspace.clone());
                return Task::perform(
                    async move {
                        let mut dialog = rfd::AsyncFileDialog::new().set_title("Choisir le dossier de travail");
                        if let Some(dir) = start {
                            dialog = dialog.set_directory(dir);
                        }
                        dialog.pick_folder().await.map(|h| h.path().to_path_buf())
                    },
                    Message::WorkspacePicked,
                );
            }
            Message::WorkspacePicked(picked) => {
                if let (Some(setup), Some(dir)) = (&mut self.workspace_setup, picked) {
                    setup.path = Some(dir);
                    setup.error = None;
                }
            }
            Message::ConfirmWorkspace => {
                if let Some(setup) = &mut self.workspace_setup {
                    let Some(dir) = setup.path.clone().filter(|p| p.is_dir()) else {
                        setup.error = Some("Choisissez un dossier existant.".into());
                        return Task::none();
                    };
                    let dir = dunce::canonicalize(&dir).unwrap_or(dir);
                    if let Err(e) = config::save_workspace(&dir) {
                        setup.error = Some(format!("{e:#}"));
                        return Task::none();
                    }
                    self.status = format!("Dossier de travail : {}", dir.display());
                    self.workspace = Some(dir);
                    self.workspace_setup = None;
                    self.refresh_projects();
                }
            }
            Message::CancelWorkspaceSetup => {
                if self.workspace.is_some() {
                    self.workspace_setup = None;
                }
            }
            Message::InputChanged(s) => {
                if let Some(conv) = self.conv_mut() {
                    conv.input = s;
                }
            }
            Message::Send => return self.send(),
            Message::Stop => {
                if let Some(conv) = self.conv_mut() {
                    if let Some(h) = conv.running.take() {
                        h.abort();
                        agent::repair(&conv.history);
                        conv.chat.push(ChatItem::Error("Interrompu.".into()));
                        self.status = "Interrompu".into();
                        if let Some(root) = self.root.clone() {
                            self.save_conv(&root);
                        }
                    }
                }
            }
            Message::NewChat => {
                if let Some(conv) = self.conv_mut() {
                    if let Some(h) = conv.running.take() {
                        h.abort();
                    }
                    let draft = std::mem::take(&mut conv.input);
                    let attachments = std::mem::take(&mut conv.attachments);
                    *conv = Conversation { input: draft, attachments, ..Conversation::default() };
                    self.status = "Nouvelle conversation".into();
                    if let Some(root) = self.root.clone() {
                        self.save_conv(&root);
                    }
                }
            }
            Message::ToggleTool(id) => {
                if let Some(conv) = self.conv_mut() {
                    for item in &mut conv.chat {
                        if let ChatItem::Tool { id: i, expanded, .. } = item {
                            if *i == id {
                                *expanded = !*expanded;
                            }
                        }
                    }
                }
            }
            Message::Agent(project, ev) => self.on_agent_event(project, ev),
            Message::PasteImage(explicit) => {
                // Only when the chat is visible (not on the setup screens).
                let Some(root) = self.root.clone() else { return Task::none() };
                if self.key_setup.is_some() || self.workspace_setup.is_some() {
                    return Task::none();
                }
                return Task::perform(clipboard::read_image(), move |r| {
                    Message::ImagePasted(root.clone(), explicit, r)
                });
            }
            Message::ImagePasted(project, explicit, result) => match result {
                Ok(att) => {
                    self.status = format!("Image jointe ({}×{})", att.width, att.height);
                    self.convs.entry(project).or_default().attachments.push(att);
                }
                Err(e) if explicit => self.status = e,
                Err(_) => {}
            },
            Message::RemoveAttachment(i) => {
                if let Some(conv) = self.conv_mut() {
                    if i < conv.attachments.len() {
                        conv.attachments.remove(i);
                    }
                }
            }
            Message::KeyInputChanged(s) => {
                if let Some(setup) = &mut self.key_setup {
                    setup.input = s;
                    setup.error = None;
                }
            }
            Message::SubmitKey => {
                if let Some(setup) = &mut self.key_setup {
                    let key = setup.input.trim().to_string();
                    if key.is_empty() || setup.checking {
                        return Task::none();
                    }
                    setup.checking = true;
                    setup.error = None;
                    return Task::perform(config::validate(key), Message::KeyValidated);
                }
            }
            Message::KeyValidated(result) => {
                if let Some(setup) = &mut self.key_setup {
                    setup.checking = false;
                    match result {
                        Ok(()) => {
                            let key = setup.input.trim().to_string();
                            match config::save_api_key(&key) {
                                Ok(_) => {
                                    self.api_key = Some((key, config::KeySource::Saved));
                                    self.key_setup = None;
                                    self.status = "Clé API enregistrée".into();
                                }
                                Err(e) => setup.error = Some(format!("{e:#}")),
                            }
                        }
                        Err(e) => setup.error = Some(e),
                    }
                }
            }
            Message::OpenKeysPage => {
                if let Err(e) = open::that(config::KEYS_URL) {
                    self.status = format!("Impossible d'ouvrir le navigateur : {e}");
                }
            }
            Message::ChangeKey => {
                if self.running_count() == 0 {
                    self.key_setup = Some(KeySetup::default());
                } else {
                    self.status = "Attendez la fin des agents en cours pour changer de clé".into();
                }
            }
            Message::CancelKeySetup => {
                if self.api_key.is_some() {
                    self.key_setup = None;
                }
            }
            Message::MediaTick => return Task::perform(media::current(), Message::MediaUpdated),
            Message::MediaUpdated(np) => self.now_playing = np,
            Message::MediaControl(cmd) => return Task::perform(media::control(cmd), Message::MediaUpdated),
            Message::CloseRequested(id) => {
                for conv in self.convs.values_mut() {
                    if let Some(h) = conv.running.take() {
                        h.abort();
                        agent::repair(&conv.history);
                    }
                }
                let projects: Vec<PathBuf> = self.convs.keys().cloned().collect();
                for p in projects {
                    self.save_conv(&p);
                }
                return iced::window::close(id);
            }
        }
        Task::none()
    }

    fn send(&mut self) -> Task<Message> {
        let Some(root) = self.root.clone() else {
            self.status = "Sélectionnez d'abord un projet dans la liste de gauche".into();
            return Task::none();
        };
        let Some((api_key, _)) = self.api_key.clone() else {
            self.key_setup = Some(KeySetup::default());
            return Task::none();
        };
        let conv = self.convs.entry(root.clone()).or_default();
        let prompt = conv.input.trim().to_string();
        if (prompt.is_empty() && conv.attachments.is_empty()) || conv.running.is_some() {
            return Task::none();
        }

        conv.input.clear();
        let attachments = std::mem::take(&mut conv.attachments);
        let thumbs = attachments.iter().map(|a| a.handle.clone()).collect();
        let images: Vec<String> = attachments.into_iter().map(|a| a.png_base64).collect();
        conv.chat.push(ChatItem::User(prompt.clone(), thumbs));

        let cfg = agent::Config { api_key, root: root.clone(), effort: "high".into() };
        let history = conv.history.clone();
        let stream = iced::stream::channel(256, move |out| agent::run(cfg, history, prompt, images, out));
        let (task, handle) =
            Task::run(stream, move |ev| Message::Agent(root.clone(), ev)).abortable();
        conv.running = Some(handle);
        task
    }

    /// Routes an agent event to its project's conversation, displayed or not.
    fn on_agent_event(&mut self, project: PathBuf, ev: AgentEvent) {
        let visible = self.root.as_ref() == Some(&project);
        let Some(conv) = self.convs.get_mut(&project) else { return };
        match ev {
            AgentEvent::TextDelta(t) => match conv.chat.last_mut() {
                Some(ChatItem::Assistant(s)) => s.push_str(&t),
                _ => conv.chat.push(ChatItem::Assistant(t)),
            },
            AgentEvent::ThinkingDelta(t) => match conv.chat.last_mut() {
                Some(ChatItem::Thinking(s)) => s.push_str(&t),
                _ => conv.chat.push(ChatItem::Thinking(t)),
            },
            AgentEvent::ToolStarted { id, name } => conv.chat.push(ChatItem::Tool {
                id,
                name,
                summary: "…".into(),
                output: None,
                is_error: false,
                diff: None,
                expanded: false,
            }),
            AgentEvent::ToolCall { id, name, input } => {
                let s = summarize(&name, &input);
                if let Some(ChatItem::Tool { summary, .. }) = conv.find_tool(&id) {
                    *summary = s;
                }
            }
            AgentEvent::ToolResult { id, output: out, is_error: err, diff: d } => {
                if let Some(ChatItem::Tool { output, is_error, diff, expanded, .. }) = conv.find_tool(&id) {
                    *expanded = d.is_some() || err;
                    *output = Some(out);
                    *is_error = err;
                    *diff = d;
                }
            }
            AgentEvent::Usage { input, output, cache_read } => {
                conv.tokens.0 += input;
                conv.tokens.1 += output;
                conv.tokens.2 += cache_read;
            }
            AgentEvent::Error(e) => conv.chat.push(ChatItem::Error(e)),
            AgentEvent::Done => {
                conv.running = None;
                if !visible {
                    conv.unread = true;
                    self.status = format!("{} : l'agent a terminé", projects::name(&project));
                }
                self.save_conv(&project);
            }
        }
    }

    /// Ctrl+V anywhere (even while the text field is focused, which captures
    /// the key to paste text) also tries to paste an image.
    fn subscription(&self) -> iced::Subscription<Message> {
        let media = iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::MediaTick);
        let paste = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Character(c),
                modifiers,
                ..
            }) if modifiers.command() && c.as_str().eq_ignore_ascii_case("v") => {
                Some(Message::PasteImage(false))
            }
            _ => None,
        });
        let close = iced::window::close_requests().map(Message::CloseRequested);
        iced::Subscription::batch([paste, media, close])
    }

    // -----------------------------------------------------------------------
    // View
    // -----------------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            text("ADE").size(15).font(BOLD),
            text(self.root.as_ref().map(|r| r.display().to_string()).unwrap_or_else(|| "Aucun projet".into()))
                .size(12)
                .color(MUTED),
            horizontal_space(),
            self.view_now_playing(),
            self.view_key_status(),
            button(text("Nouvelle conversation").size(12)).on_press(Message::NewChat).style(button::text),
        ]
        .spacing(12)
        .padding([6, 12])
        .align_y(Alignment::Center);

        let main: Element<'_, Message> = if let Some(setup) = &self.key_setup {
            self.view_key_setup(setup)
        } else if let Some(setup) = &self.workspace_setup {
            self.view_workspace_setup(setup)
        } else {
            row![
                container(self.view_projects()).width(240).height(Length::Fill),
                vertical_rule(1),
                container(self.view_chat()).width(Length::Fill).height(Length::Fill),
            ]
            .height(Length::Fill)
            .into()
        };

        let t = self.conv().map(|c| c.tokens).unwrap_or_default();
        let current = match self.conv() {
            Some(c) if c.running.is_some() => "L'agent travaille…".to_string(),
            _ => self.status.clone(),
        };
        let active = match self.running_count() {
            0 => String::new(),
            1 => "1 agent actif".into(),
            n => format!("{n} agents actifs"),
        };
        let status = row![
            text(current).size(11).color(MUTED),
            horizontal_space(),
            text(active).size(11).color(BLUE),
            text(format!("{}  ·  in {}  ·  out {}  ·  cache {}", agent::MODEL, t.0, t.1, t.2))
                .size(11)
                .color(MUTED),
        ]
        .spacing(16)
        .padding([4, 12]);

        column![header, horizontal_rule(1), main, horizontal_rule(1), status].into()
    }

    fn view_projects(&self) -> Element<'_, Message> {
        let head = row![
            text("PROJETS").size(11).font(BOLD).color(MUTED),
            horizontal_space(),
            button(text("+").size(15))
                .on_press_maybe(self.workspace.is_some().then_some(Message::NewProject))
                .style(button::text)
                .padding([0, 6]),
            button(text("↻").size(13).font(SYMBOL)).on_press(Message::RefreshProjects).style(button::text).padding([0, 6]),
        ]
        .padding([8, 12])
        .align_y(Alignment::Center);

        let new_form: Option<Element<'_, Message>> = self.new_project.as_ref().map(|np| {
            let mut form = column![
                text_input("Nom du nouveau projet", &np.name)
                    .id(new_project_input_id())
                    .on_input(Message::NewProjectNameChanged)
                    .on_submit(Message::CreateProject)
                    .size(13)
                    .padding(6),
                row![
                    button(text("Créer").size(12))
                        .on_press_maybe((!np.name.trim().is_empty()).then_some(Message::CreateProject))
                        .style(button::primary)
                        .padding([4, 10]),
                    button(text("Annuler").size(12))
                        .on_press(Message::CancelNewProject)
                        .style(button::secondary)
                        .padding([4, 10]),
                ]
                .spacing(6),
            ]
            .spacing(6)
            .padding([4, 12]);
            if let Some(err) = &np.error {
                form = form.push(text(err).size(11).color(RED));
            }
            form.into()
        });

        let list: Element<'_, Message> = if self.projects.is_empty() {
            container(text("Aucun dossier dans le dossier de travail.").size(12).color(MUTED))
                .padding([4, 12])
                .into()
        } else {
            let items = self.projects.iter().map(|p| {
                let selected = self.root.as_deref() == Some(p.as_path());
                let conv = self.convs.get(p);
                let busy = conv.is_some_and(|c| c.running.is_some());
                let unread = conv.is_some_and(|c| c.unread);
                let has_chat = conv.is_some_and(|c| !c.chat.is_empty());
                // ◐ working · ● finished in the background · ○ has a conversation
                let (marker, marker_color) = if busy {
                    ("◐", BLUE)
                } else if unread {
                    ("●", GREEN)
                } else if has_chat {
                    ("○", MUTED)
                } else {
                    (" ", MUTED)
                };
                let label = row![
                    text(marker).size(12).font(SYMBOL).color(marker_color).width(14),
                    text(projects::name(p))
                        .size(13)
                        .color(if selected { BLUE } else { TEXT })
                        .font(if selected { BOLD } else { Font::DEFAULT }),
                ]
                .spacing(4)
                .align_y(Alignment::Center);
                button(label)
                    .on_press(Message::SelectProject(p.clone()))
                    .style(button::text)
                    .padding([4, 12])
                    .width(Length::Fill)
                    .into()
            });
            scrollable(Column::with_children(items)).height(Length::Fill).into()
        };

        let footer = column![
            text(self.workspace.as_ref().map(|w| w.display().to_string()).unwrap_or_default())
                .size(11)
                .color(MUTED),
            button(text("Changer de dossier de travail").size(11))
                .on_press(Message::ChangeWorkspace)
                .style(button::text)
                .padding(0),
        ]
        .spacing(4)
        .padding(12);

        column![head]
            .push_maybe(new_form)
            .push(container(list).height(Length::Fill))
            .push(horizontal_rule(1))
            .push(footer)
            .into()
    }

    fn view_workspace_setup<'a>(&'a self, setup: &'a WorkspaceSetup) -> Element<'a, Message> {
        let first_launch = self.workspace.is_none();
        let chosen = setup.path.as_ref().map(|p| p.display().to_string());
        let count = setup.path.as_deref().map(|p| projects::list(p).len());

        let path_row = row![
            container(
                text(chosen.clone().unwrap_or_else(|| "Aucun dossier choisi".into()))
                    .size(13)
                    .font(Font::MONOSPACE)
                    .color(if chosen.is_some() { TEXT } else { MUTED })
            )
            .padding(10)
            .width(Length::Fill)
            .style(container::bordered_box),
            button(text("Parcourir…").size(13)).on_press(Message::PickWorkspace).style(button::secondary).padding([9, 14]),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut actions = row![button(text("Valider").size(13))
            .on_press_maybe(setup.path.is_some().then_some(Message::ConfirmWorkspace))
            .style(button::primary)
            .padding([8, 16])]
        .spacing(8);
        if !first_launch {
            actions = actions.push(
                button(text("Annuler").size(13))
                    .on_press(Message::CancelWorkspaceSetup)
                    .style(button::secondary)
                    .padding([8, 16]),
            );
        }

        let mut form = column![
            text(if first_launch { "Où se trouve votre dossier de travail ?" } else { "Changer de dossier de travail" })
                .size(22),
            text("Chaque dossier qu'il contient apparaîtra comme un projet dans la liste de gauche.")
                .size(13)
                .color(MUTED),
            path_row,
        ]
        .spacing(14)
        .max_width(560);

        if let Some(n) = count {
            form = form.push(text(format!("{n} dossier(s) trouvé(s)")).size(12).color(MUTED));
        }
        if let Some(err) = &setup.error {
            form = form.push(text(err).size(12).color(RED));
        }
        form = form.push(actions);

        container(form).center(Length::Fill).padding(24).into()
    }

    fn view_now_playing(&self) -> Element<'_, Message> {
        let Some(np) = &self.now_playing else { return row![].into() };
        let icon = |glyph: &'static str, cmd: media::Command| {
            button(text(glyph).size(12).font(ICONS).color(TEXT))
                .on_press(Message::MediaControl(cmd))
                .style(button::text)
                .padding([2, 4])
        };
        let label = if np.artist.trim().is_empty() {
            np.title.clone()
        } else {
            format!("{} — {}", np.title, np.artist)
        };
        row![
            text("♪").size(12).font(SYMBOL).color(if np.playing { GREEN } else { MUTED }),
            text(clip(&label, 60)).size(12).color(if np.playing { TEXT } else { MUTED }),
            icon(ICON_PREV, media::Command::Previous),
            icon(if np.playing { ICON_PAUSE } else { ICON_PLAY }, media::Command::PlayPause),
            icon(ICON_NEXT, media::Command::Next),
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
    }

    fn view_key_status(&self) -> Element<'_, Message> {
        match &self.api_key {
            Some((_, config::KeySource::Env)) => {
                row![
                    text("●").size(12).font(SYMBOL).color(GREEN),
                    text("Clé API (ANTHROPIC_API_KEY)").size(12).color(GREEN),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into()
            }
            Some((key, config::KeySource::Saved)) => {
                let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                button(
                    row![
                        text("●").size(12).font(SYMBOL).color(GREEN),
                        text(format!("Clé API …{tail}")).size(12).color(GREEN),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                    .on_press(Message::ChangeKey)
                    .style(button::text)
                    .into()
            }
            None => button(text("Ajouter une clé API").size(12))
                .on_press(Message::ChangeKey)
                .style(button::primary)
                .into(),
        }
    }

    fn view_key_setup<'a>(&'a self, setup: &'a KeySetup) -> Element<'a, Message> {
        let first_launch = self.api_key.is_none();
        let can_submit = !setup.input.trim().is_empty() && !setup.checking;

        let mut actions = row![button(text(if setup.checking { "Vérification…" } else { "Valider" }).size(13))
            .on_press_maybe(can_submit.then_some(Message::SubmitKey))
            .style(button::primary)
            .padding([8, 16])]
        .spacing(8);
        if !first_launch {
            actions = actions.push(
                button(text("Annuler").size(13))
                    .on_press(Message::CancelKeySetup)
                    .style(button::secondary)
                    .padding([8, 16]),
            );
        }

        let mut form = column![
            text(if first_launch { "Bienvenue dans ADE" } else { "Changer de clé API" }).size(22),
            text("ADE utilise l'API Claude. Entrez une clé API pour activer l'agent.")
                .size(13)
                .color(MUTED),
            text_input("sk-ant-api03-…", &setup.input)
                .secure(true)
                .on_input(Message::KeyInputChanged)
                .on_submit(Message::SubmitKey)
                .size(14)
                .padding(10),
        ]
        .spacing(14)
        .max_width(520);

        if let Some(err) = &setup.error {
            form = form.push(text(err).size(12).color(RED));
        }
        form = form.push(actions).push(horizontal_rule(1)).push(
            row![
                text("Pas encore de clé ?").size(13).color(MUTED),
                button(text("Créer une clé API sur platform.claude.com ↗").size(13).color(BLUE))
                    .on_press(Message::OpenKeysPage)
                    .style(button::text)
                    .padding(0),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        let location = config::path()
            .map(|p| format!("La clé sera vérifiée puis enregistrée dans {}", p.display()))
            .unwrap_or_default();
        form = form.push(text(location).size(11).color(MUTED));

        container(form).center(Length::Fill).padding(24).into()
    }

    fn view_chat(&self) -> Element<'_, Message> {
        let (Some(root), Some(conv)) = (&self.root, self.conv()) else {
            return container(text("Sélectionnez un projet dans la liste de gauche pour discuter avec Claude.").size(14).color(MUTED))
                .center(Length::Fill)
                .padding(24)
                .into();
        };
        let items = conv.chat.iter().map(|item| self.view_item(item));
        // Centered reading column: long lines are hard to read on wide windows.
        let log = scrollable(
            container(Column::with_children(items).spacing(12).padding([16, 24]).max_width(900))
                .center_x(Length::Fill),
        )
        .anchor_bottom()
        .height(Length::Fill);

        let action = if conv.running.is_some() {
            button(text("Stop").size(13)).on_press(Message::Stop).style(button::danger)
        } else {
            let can_send = !conv.input.trim().is_empty() || !conv.attachments.is_empty();
            button(text("Envoyer").size(13))
                .on_press_maybe(can_send.then_some(Message::Send))
                .style(button::primary)
        };
        let placeholder = format!("Demandez quelque chose à l'agent sur {}… (Ctrl+V pour coller une image)", projects::name(root));
        let input = row![
            text_input(&placeholder, &conv.input)
                .on_input(Message::InputChanged)
                .on_submit(Message::Send)
                .size(13)
                .padding(8),
            button(text("Image").size(13))
                .on_press(Message::PasteImage(true))
                .style(button::secondary)
                .padding([8, 12]),
            action.padding([8, 14]),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut bottom = column![].spacing(8).padding(10);
        if !conv.attachments.is_empty() {
            let thumbs = conv.attachments.iter().enumerate().map(|(i, a)| {
                column![
                    iced::widget::image(a.handle.clone()).height(72),
                    button(text("Retirer").size(11).color(MUTED))
                        .on_press(Message::RemoveAttachment(i))
                        .style(button::text)
                        .padding(0),
                ]
                .spacing(2)
                .align_x(Alignment::Center)
                .into()
            });
            bottom = bottom.push(iced::widget::Row::with_children(thumbs).spacing(8));
        }
        bottom = bottom.push(input);

        column![log, horizontal_rule(1), bottom].into()
    }

    fn view_item<'a>(&'a self, item: &'a ChatItem) -> Element<'a, Message> {
        match item {
            ChatItem::User(s, images) => {
                let mut col = column![].spacing(8);
                if !images.is_empty() {
                    let thumbs = images
                        .iter()
                        .map(|h| iced::widget::image(h.clone()).height(160).into());
                    col = col.push(iced::widget::Row::with_children(thumbs).spacing(8));
                }
                if !s.is_empty() {
                    col = col.push(text(s).size(13));
                }
                container(col)
                    .padding(10)
                    .width(Length::Fill)
                    .style(code_box)
                    .into()
            }
            ChatItem::Assistant(s) => view_assistant(s.trim()),
            ChatItem::Thinking(s) => text(clip(s.trim(), 600)).size(12).font(ITALIC).color(MUTED).into(),
            ChatItem::Error(e) => text(e).size(12).color(RED).into(),
            ChatItem::Tool { id, name, summary, output, is_error, diff, expanded } => {
                let state = match (output, is_error) {
                    (None, _) => text("…").color(MUTED),
                    (Some(_), true) => text("✗").font(SYMBOL).color(RED),
                    (Some(_), false) => text("✓").font(SYMBOL).color(GREEN),
                };
                let head = button(
                    row![
                        state.size(12),
                        text(name).size(12).font(Font::MONOSPACE).color(BLUE),
                        text(summary).size(12).font(Font::MONOSPACE).color(MUTED),
                    ]
                    .spacing(8),
                )
                .on_press(Message::ToggleTool(id.clone()))
                .style(button::text)
                .padding(0);

                let mut col = column![head].spacing(6);
                if *expanded {
                    if let Some(d) = diff {
                        col = col.push(view_diff(d));
                    } else if let Some(o) = output {
                        col = col.push(
                            container(text(clip(o, MAX_DISPLAY_CHARS)).size(11).font(Font::MONOSPACE))
                                .padding(8)
                                .width(Length::Fill)
                                .style(code_box),
                        );
                    }
                }
                col.into()
            }
        }
    }
}

fn view_diff(diff: &str) -> Element<'_, Message> {
    let lines = diff.lines().take(400).map(|l| {
        let color = if l.starts_with("+++") || l.starts_with("---") {
            MUTED
        } else if l.starts_with('+') {
            GREEN
        } else if l.starts_with('-') {
            RED
        } else if l.starts_with("@@") {
            BLUE
        } else {
            Color::from_rgb(0.75, 0.76, 0.80)
        };
        text(l).size(11).font(Font::MONOSPACE).color(color).into()
    });
    container(Column::with_children(lines))
        .padding(8)
        .width(Length::Fill)
        .style(code_box)
        .into()
}

/// Dark, bordered background used for every block of code shown in the chat.
fn code_box(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb(0.08, 0.085, 0.10).into()),
        border: iced::Border {
            color: Color::from_rgb(0.22, 0.23, 0.27),
            width: 1.0,
            radius: 6.0.into(),
        },
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

/// Renders an assistant message, putting fenced ``` code blocks in a dark monospace box.
fn view_assistant(s: &str) -> Element<'_, Message> {
    let mut col = column![].spacing(8);
    let mut prose: Vec<&str> = Vec::new();
    let mut code: Option<(&str, Vec<&str>)> = None;

    let flush_prose = |col: Column<'static, Message>, prose: &mut Vec<&str>| {
        let joined = prose.join("\n");
        prose.clear();
        let trimmed = joined.trim_matches('\n');
        if trimmed.is_empty() {
            col
        } else {
            col.push(text(trimmed.to_string()).size(13))
        }
    };
    let code_block = |lang: &str, lines: &[&str]| -> Element<'static, Message> {
        let mut inner = column![].spacing(4);
        if !lang.is_empty() {
            inner = inner.push(text(lang.to_string()).size(10).color(MUTED));
        }
        inner = inner.push(text(lines.join("\n")).size(12).font(Font::MONOSPACE));
        container(inner).padding([8, 10]).width(Length::Fill).style(code_box).into()
    };

    for line in s.lines() {
        let fence = line.trim_start().strip_prefix("```");
        match (&mut code, fence) {
            (None, Some(lang)) => {
                col = flush_prose(col, &mut prose);
                code = Some((lang.trim(), Vec::new()));
            }
            (Some((lang, lines)), Some(_)) => {
                col = col.push(code_block(lang, lines));
                code = None;
            }
            (Some((_, lines)), None) => lines.push(line),
            (None, None) => prose.push(line),
        }
    }
    // Unterminated block (e.g. message still streaming): still show it as code.
    if let Some((lang, lines)) = &code {
        col = col.push(code_block(lang, lines));
    }
    col = flush_prose(col, &mut prose);
    col.into()
}

/// Rebuilds the chat view of a saved conversation from its API history.
fn chat_from_history(history: &[serde_json::Value], diffs: &HashMap<String, String>) -> Vec<ChatItem> {
    use base64::Engine;
    let mut chat: Vec<ChatItem> = Vec::new();
    for msg in history {
        let blocks = match &msg["content"] {
            serde_json::Value::String(s) => vec![serde_json::json!({ "type": "text", "text": s })],
            serde_json::Value::Array(a) => a.clone(),
            _ => continue,
        };
        if msg["role"] == "user" {
            let mut prompt = String::new();
            let mut images = Vec::new();
            let mut is_prompt = false;
            for b in &blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        is_prompt = true;
                        prompt.push_str(b["text"].as_str().unwrap_or_default());
                    }
                    Some("image") => {
                        is_prompt = true;
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD
                            .decode(b["source"]["data"].as_str().unwrap_or_default())
                        {
                            images.push(iced::widget::image::Handle::from_bytes(bytes));
                        }
                    }
                    Some("tool_result") => {
                        let id = b["tool_use_id"].as_str().unwrap_or_default();
                        let out = match &b["content"] {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Array(a) => a
                                .iter()
                                .filter_map(|c| c["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        let err = b["is_error"].as_bool().unwrap_or(false);
                        let d = diffs.get(id).cloned();
                        if let Some(ChatItem::Tool { output, is_error, diff, expanded, .. }) = chat
                            .iter_mut()
                            .rev()
                            .find(|i| matches!(i, ChatItem::Tool { id: x, .. } if x == id))
                        {
                            *expanded = d.is_some() || err;
                            *output = Some(out);
                            *is_error = err;
                            *diff = d;
                        }
                    }
                    _ => {}
                }
            }
            if is_prompt {
                chat.push(ChatItem::User(prompt, images));
            }
        } else {
            for b in &blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        let t = b["text"].as_str().unwrap_or_default();
                        match chat.last_mut() {
                            Some(ChatItem::Assistant(s)) => s.push_str(t),
                            _ => chat.push(ChatItem::Assistant(t.to_string())),
                        }
                    }
                    Some("thinking") => {
                        let t = b["thinking"].as_str().unwrap_or_default();
                        if t.is_empty() {
                            continue;
                        }
                        match chat.last_mut() {
                            Some(ChatItem::Thinking(s)) => s.push_str(t),
                            _ => chat.push(ChatItem::Thinking(t.to_string())),
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or_default().to_string();
                        chat.push(ChatItem::Tool {
                            id: b["id"].as_str().unwrap_or_default().to_string(),
                            summary: summarize(&name, &b["input"]),
                            name,
                            output: None,
                            is_error: false,
                            diff: None,
                            expanded: false,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    chat
}

fn summarize(name: &str, input: &serde_json::Value) -> String {
    let s = match name {
        "run_command" => input["command"].as_str().unwrap_or_default().to_string(),
        "search" => format!(
            "/{}/ {}",
            input["pattern"].as_str().unwrap_or_default(),
            input["path"].as_str().unwrap_or_default()
        ),
        _ => input["path"].as_str().unwrap_or_default().to_string(),
    };
    clip(&s, 80)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
