use ansi_parser::{AnsiParser, AnsiSequence, Output};
use directories_next::ProjectDirs;
use iced::executor;
use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{
    advanced::subscription::{EventStream, Recipe},
    advanced::Hasher,
    event::{self, Status},
    futures::stream::{BoxStream, StreamExt},
    theme, window, Alignment, Application, Background, Border, Color, Command, Element, Event,
    Length, Settings, Subscription, Theme,
};
use rfd::AsyncFileDialog;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher as StdHasher},
    io,
    path::PathBuf,
    process::Stdio,
};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

// --- Константы ---
const MAX_LOG_LINES: usize = 500;
const CONFIG_FILE_NAME: &str = "launcher_settings.json";
const BUTTON_TEXT_COLOR: Color = Color::WHITE;

// --- Структура для хранения настроек ---
#[derive(Debug, Serialize, Deserialize, Clone)]
struct AppSettings {
    executable_path: Option<PathBuf>,
    api_key: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            executable_path: None,
            api_key: String::new(),
        }
    }
}

// --- Структура для сегмента ANSI ---
#[derive(Debug, Clone, PartialEq)]
pub struct AnsiSegment {
    text: String,
    color: Option<Color>,
    // Можно добавить другие атрибуты стиля (жирный, подчеркнутый), если нужно
}

// --- Состояние приложения ---
pub struct Launcher {
    settings: AppSettings,
    is_running: bool,
    logs: VecDeque<Vec<AnsiSegment>>,
    show_settings: bool,
    config_path: Option<PathBuf>,
    subscription_id_counter: u64,
    subscription_id: Option<u64>,
    actual_pid: Option<u32>,
    close_requested: bool,
}

// --- Сообщения для обновления состояния ---
#[derive(Debug, Clone)]
pub enum Message {
    SettingsButtonPressed,
    StartButtonPressed,
    StopButtonPressed,
    SelectExecutablePath,
    ApiKeyChanged(String),
    CloseSettingsPressed,
    ExecutablePathSelected(Result<Option<PathBuf>, String>),
    SettingsLoaded(Result<AppSettings, String>),
    SettingsSaved(Result<(), String>),
    ProcessActualPid(u32),
    ProcessOutput(String),
    ProcessTerminated(i32),
    ProcessError(String),
    ProcessKillResult(Result<(), String>),
    EventOccurred(iced::Event),
}

// --- Функции для работы с конфигурацией ---
fn get_config_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "TradingStar", "TradingStar3Launcher").map(|dirs| {
        let config_dir = dirs.config_dir();
        config_dir.join(CONFIG_FILE_NAME)
    })
}

async fn load_settings(path: Option<PathBuf>) -> Result<AppSettings, String> {
    let path = path.ok_or_else(|| "Не удалось определить путь к конфигурации".to_string())?;
    if !path.exists() {
        return Ok(AppSettings::default());
    }
    let content = fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Ошибка чтения файла конфигурации {:?}: {}", path, e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("Ошибка парсинга файла конфигурации {:?}: {}", path, e))
}

async fn save_settings(path: Option<PathBuf>, settings: AppSettings) -> Result<(), String> {
    let path = path.ok_or_else(|| "Не удалось определить путь к конфигурации".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Не удалось создать директорию {:?}: {}", parent, e))?;
    }
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Ошибка сериализации настроек: {}", e))?;
    let mut file = fs::File::create(&path).await.map_err(|e| {
        format!(
            "Не удалось создать/открыть файл конфигурации {:?}: {}",
            path, e
        )
    })?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|e| format!("Не удалось записать в файл конфигурации {:?}: {}", path, e))?;
    Ok(())
}

async fn select_executable_file() -> Result<Option<PathBuf>, String> {
    let file_handle = AsyncFileDialog::new()
        .set_title("Выберите исполняемый файл...")
        .set_directory("/")
        .pick_file()
        .await;

    match file_handle {
        Some(handle) => Ok(Some(handle.path().to_path_buf())),
        None => Ok(None),
    }
}

async fn kill_process(pid: u32) -> Result<(), String> {
    println!("[kill_process] Попытка завершить процесс с PID: {}", pid);

    #[cfg(unix)]
    {
        println!("[kill_process] Выполнение команды: kill {}", pid);
        let kill_cmd = TokioCommand::new("kill")
            .arg(pid.to_string())
            .output() // Используем output() чтобы получить stdout/stderr и статус
            .await;
        match kill_cmd {
            Ok(output) => {
                println!("[kill_process] Статус kill: {}", output.status);
                if !output.stdout.is_empty() {
                    println!(
                        "[kill_process] kill stdout: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
                if !output.stderr.is_empty() {
                    println!(
                        "[kill_process] kill stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                if output.status.success() {
                    println!(
                        "[kill_process] Команда kill успешно завершена для PID: {}",
                        pid
                    );
                    Ok(())
                } else {
                    Err(format!(
                        "Команда kill для PID {} завершилась с кодом: {}. Stderr: {}",
                        pid,
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            Err(e) => {
                let error_msg = format!("Ошибка выполнения команды kill для PID {}: {}", pid, e);
                println!("[kill_process] {}", error_msg);
                Err(error_msg)
            }
        }
    }

    #[cfg(windows)]
    {
        println!(
            "[kill_process] Выполнение команды: taskkill /F /PID {}",
            pid
        );
        let kill_cmd = TokioCommand::new("taskkill")
            .arg("/F")
            .arg("/PID")
            .arg(pid.to_string())
            .output() // Используем output()
            .await;

        match kill_cmd {
            Ok(output) => {
                println!("[kill_process] Статус taskkill: {}", output.status);
                if !output.stdout.is_empty() {
                    println!(
                        "[kill_process] taskkill stdout: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
                if !output.stderr.is_empty() {
                    println!(
                        "[kill_process] taskkill stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                if output.status.success() {
                    // На Windows taskkill может вернуть успех, даже если процесс уже не существует
                    // Проверяем stdout на наличие сообщения об успехе
                    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
                    if stdout.contains(&format!("pid {} ", pid)) || stdout.contains("success") {
                        println!(
                            "[kill_process] Команда taskkill успешно завершена для PID: {}",
                            pid
                        );
                        Ok(())
                    } else {
                        // Возможно, процесс уже был завершен до вызова taskkill
                        println!("[kill_process] taskkill stdout не содержит подтверждения успеха для PID {}. Возможно, процесс уже был завершен.", pid);
                        // Считаем это успехом, так как цель - чтобы процесса не было
                        Ok(())
                    }
                } else {
                    Err(format!(
                        "Команда taskkill для PID {} завершилась с кодом: {}. Stderr: {}",
                        pid,
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            Err(e) => {
                let error_msg =
                    format!("Ошибка выполнения команды taskkill для PID {}: {}", pid, e);
                println!("[kill_process] {}", error_msg);
                Err(error_msg)
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let error_msg = "Остановка процесса не поддерживается на этой ОС.".to_string();
        println!("[kill_process] {}", error_msg);
        Err(error_msg)
    }
}

// --- Вспомогательная функция для конвертации ANSI цвета ---
fn ansi_to_iced_color(code: u8) -> Color {
    match code {
        // Стандартные цвета
        30 => Color::BLACK,                       // Black
        31 => Color::from_rgb8(0xCD, 0x5C, 0x5C), // Red (IndianRed)
        32 => Color::from_rgb8(0x90, 0xEE, 0x90), // Green (LightGreen)
        33 => Color::from_rgb8(0xFF, 0xD7, 0x00), // Yellow (Gold)
        34 => Color::from_rgb8(0x46, 0x82, 0xB4), // Blue (SteelBlue)
        35 => Color::from_rgb8(0xBA, 0x55, 0xD3), // Magenta (MediumOrchid)
        36 => Color::from_rgb8(0x40, 0xE0, 0xD0), // Cyan (Turquoise)
        37 => Color::from_rgb8(0xD3, 0xD3, 0xD3), // White (LightGray)
        // Яркие цвета (часто используются)
        90 => Color::from_rgb8(0x80, 0x80, 0x80), // Bright Black (Gray)
        91 => Color::from_rgb8(0xFF, 0x00, 0x00), // Bright Red
        92 => Color::from_rgb8(0x00, 0xFF, 0x00), // Bright Green (Lime)
        93 => Color::from_rgb8(0xFF, 0xFF, 0x00), // Bright Yellow
        94 => Color::from_rgb8(0x00, 0x00, 0xFF), // Bright Blue
        95 => Color::from_rgb8(0xFF, 0x00, 0xFF), // Bright Magenta (Fuchsia)
        96 => Color::from_rgb8(0x00, 0xFF, 0xFF), // Bright Cyan (Aqua)
        97 => Color::WHITE,                       // Bright White
        // Сброс (используем цвет по умолчанию - None в AnsiSegment)
        0 | 39 | 49 => Color::WHITE, // Treat reset like default terminal color
        // Другие коды пока игнорируем или возвращаем белый
        _ => Color::WHITE,
    }
}

// --- ProcessListener Recipe ---
#[derive(Debug)]
struct ProcessListener {
    id: u64,
    path: PathBuf,
    api_key: String,
}
impl ProcessListener {
    fn new(id: u64, path: PathBuf, api_key: String) -> Self {
        Self { id, path, api_key }
    }
}
impl Recipe for ProcessListener {
    type Output = Message;

    fn hash(&self, state: &mut Hasher) {
        std::any::TypeId::of::<Self>().hash(state);
        self.id.hash(state);
    }

    fn stream(self: Box<Self>, _input: EventStream) -> BoxStream<'static, Self::Output> {
        let (sender, receiver) = mpsc::channel(100);

        let path = self.path;
        let api_key = self.api_key;

        tokio::spawn(async move {
            let mut child: Child;
            let actual_pid: u32;
            match TokioCommand::new(&path)
                .arg("-k")
                .arg(&api_key)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(spawned_child) => {
                    child = spawned_child;
                    if let Some(pid) = child.id() {
                        actual_pid = pid;
                        if sender
                            .send(Message::ProcessActualPid(actual_pid))
                            .await
                            .is_err()
                        {
                            eprintln!("[Recipe] Failed to send actual PID");
                            return;
                        }
                    } else {
                        let _ = sender
                            .send(Message::ProcessError(
                                "Не удалось получить PID запущенного процесса.".to_string(),
                            ))
                            .await;
                        return;
                    }
                }
                Err(e) => {
                    let _ = sender
                        .send(Message::ProcessError(format!(
                            "Ошибка запуска процесса {:?}: {}",
                            path, e
                        )))
                        .await;
                    return;
                }
            }

            let stdout = child.stdout.take().expect("stdout not captured");
            let stderr = child.stderr.take().expect("stderr not captured");

            let sender_stdout = sender.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    if sender_stdout
                        .send(Message::ProcessOutput(line))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });

            let sender_stderr = sender.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    if sender_stderr
                        .send(Message::ProcessOutput(format!("STDERR: {}", line)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });

            let sender_termination = sender;
            tokio::spawn(async move {
                let message = match child.wait().await {
                    Ok(status) => Message::ProcessTerminated(status.code().unwrap_or(-1)),
                    Err(e) => Message::ProcessError(format!(
                        "Ошибка ожидания процесса PID {}: {}",
                        actual_pid, e
                    )),
                };
                let _ = sender_termination.send(message).await;
            });
        });

        ReceiverStream::new(receiver).boxed()
    }
}

// --- Реализация Application ---
impl Application for Launcher {
    type Executor = executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        let config_path = get_config_path();
        let initial_state = Launcher {
            settings: AppSettings::default(),
            is_running: false,
            logs: VecDeque::with_capacity(MAX_LOG_LINES),
            show_settings: false,
            config_path: config_path.clone(),
            subscription_id_counter: 0,
            subscription_id: None,
            actual_pid: None,
            close_requested: false,
        };
        (
            initial_state,
            Command::perform(load_settings(config_path), Message::SettingsLoaded),
        )
    }

    fn title(&self) -> String {
        String::from("TradingStar 3 Launcher")
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        let mut commands_to_batch = vec![];

        match message {
            Message::SettingsLoaded(Ok(loaded_settings)) => {
                self.settings = loaded_settings;
            }
            Message::SettingsLoaded(Err(e)) => {
                eprintln!("Ошибка загрузки настроек: {}", e);
                self.add_log(format!("Ошибка загрузки настроек: {}", e));
                self.settings = AppSettings::default();
            }
            Message::SettingsButtonPressed => {
                self.show_settings = true;
            }
            Message::CloseSettingsPressed => {
                self.show_settings = false;
            }
            Message::StartButtonPressed => {
                if self.is_running {
                    return Command::none();
                }
                if self.settings.executable_path.is_some() && !self.settings.api_key.is_empty() {
                    self.logs.clear();
                    self.add_log("Запуск процесса через подписку...".to_string());
                    self.is_running = true;
                    let new_id = self.subscription_id_counter;
                    self.subscription_id_counter += 1;
                    self.subscription_id = Some(new_id);
                    self.actual_pid = None;
                } else {
                    self.add_log("Ошибка: Проверьте путь и ключ API.".to_string());
                }
            }
            Message::StopButtonPressed => {
                if let Some(pid) = self.actual_pid.take() {
                    self.add_log(format!("Остановка процесса (PID: {})...", pid));
                    self.is_running = false;
                    self.subscription_id = None;
                    commands_to_batch.push(Command::perform(
                        kill_process(pid),
                        Message::ProcessKillResult,
                    ));
                } else {
                    self.add_log("Процесс не запущен или PID неизвестен.".to_string());
                    self.is_running = false;
                    self.subscription_id = None;
                }
            }
            Message::ProcessActualPid(pid) => {
                self.add_log(format!("Процесс успешно запущен (PID: {}).", pid));
                self.actual_pid = Some(pid);
            }
            Message::ProcessOutput(line) => {
                self.add_log(line);
            }
            Message::ProcessTerminated(exit_code) => {
                self.add_log(format!("Процесс завершился (код: {}).", exit_code));
                self.is_running = false;
                self.subscription_id = None;
                self.actual_pid = None;
                if self.close_requested {
                    commands_to_batch.push(window::close(window::Id::MAIN));
                }
            }
            Message::ProcessError(error_msg) => {
                self.add_log(error_msg);
                self.is_running = false;
                self.subscription_id = None;
                self.actual_pid = None;
                if self.close_requested {
                    commands_to_batch.push(window::close(window::Id::MAIN));
                }
            }
            Message::ProcessKillResult(result) => {
                match result {
                    Ok(_) => self.add_log("Команда остановки процесса отправлена.".to_string()),
                    Err(e) => self.add_log(format!("Ошибка отправки команды остановки: {}", e)),
                }
                self.is_running = false;
                self.subscription_id = None;
                self.actual_pid = None;
                if self.close_requested {
                    commands_to_batch.push(window::close(window::Id::MAIN));
                }
            }
            Message::SelectExecutablePath => {
                return Command::perform(select_executable_file(), Message::ExecutablePathSelected);
            }
            Message::ExecutablePathSelected(Ok(Some(path))) => {
                self.settings.executable_path = Some(path);
                self.add_log(format!(
                    "Выбран путь: {:?}",
                    self.settings.executable_path.as_ref().unwrap()
                ));
                commands_to_batch.push(Command::perform(
                    save_settings(self.config_path.clone(), self.settings.clone()),
                    Message::SettingsSaved,
                ));
            }
            Message::ExecutablePathSelected(Ok(None)) => {
                self.add_log("Выбор файла отменен.".to_string());
            }
            Message::ExecutablePathSelected(Err(e)) => {
                eprintln!("Ошибка выбора файла: {}", e);
                self.add_log(format!("Ошибка выбора файла: {}", e));
            }
            Message::ApiKeyChanged(new_key) => {
                self.settings.api_key = new_key;
                commands_to_batch.push(Command::perform(
                    save_settings(self.config_path.clone(), self.settings.clone()),
                    Message::SettingsSaved,
                ));
            }
            Message::SettingsSaved(Ok(())) => {
                println!("Настройки сохранены.");
            }
            Message::SettingsSaved(Err(e)) => {
                eprintln!("Ошибка сохранения настроек: {}", e);
                self.add_log(format!("Ошибка сохранения настроек: {}", e));
            }
            Message::EventOccurred(event) => {
                // Лог 1: Получено ли событие вообще?

                if let Event::Window(id, window::Event::CloseRequested) = event {
                    // Лог 2: Событие - это запрос на закрытие окна?
                    println!(
                        "[EventOccurred] Событие - запрос на закрытие для окна ID: {:?}",
                        id
                    );

                    if id == window::Id::MAIN {
                        // Лог 3: Окно - главное?
                        println!(
                            "[EventOccurred] Окно - главное (MAIN). Запускаем логику закрытия."
                        );

                        // --- Основная логика ---
                        self.add_log("Получен запрос на закрытие окна...".to_string());
                        self.close_requested = true;
                        if self.is_running {
                            if let Some(pid) = self.actual_pid {
                                self.add_log(format!(
                                    "Инициирована остановка процесса (PID: {}) перед закрытием.",
                                    pid
                                ));
                                commands_to_batch.push(Command::perform(
                                    kill_process(pid),
                                    Message::ProcessKillResult,
                                ));
                            } else {
                                self.add_log(
                                    "Процесс был запущен, но PID не найден. Закрытие окна."
                                        .to_string(),
                                );
                                self.is_running = false;
                                self.subscription_id = None;
                                commands_to_batch.push(window::close(window::Id::MAIN));
                            }
                        } else {
                            // Лог 5: Процесс не запущен при закрытии.
                            println!("[EventOccurred] Процесс не запущен. Запрос на немедленное закрытие.");
                            self.add_log("Процесс не запущен. Закрытие окна.".to_string());
                            commands_to_batch.push(window::close(window::Id::MAIN));
                        }
                        // --- Конец основной логики ---
                    } else {
                        // Лог 4: Окно не главное.
                        println!("[EventOccurred] Окно ID {:?} не является главным (MAIN). Игнорируем запрос.", id);
                        self.add_log(format!(
                            "Запрос на закрытие для окна {:?}, игнорируется.",
                            id
                        ));
                    }
                }
                // Если событие не Event::Window(_, window::Event::CloseRequested), оно просто игнорируется здесь
                // Можно добавить else блок для if let, если нужно логировать и другие типы событий
            }
        }
        Command::batch(commands_to_batch)
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let window_events = event::listen().map(Message::EventOccurred);

        let process_subscription = if self.is_running {
            if let Some(id) = self.subscription_id {
                if let Some(path) = self.settings.executable_path.clone() {
                    if !self.settings.api_key.is_empty() {
                        Subscription::from_recipe(ProcessListener::new(
                            id,
                            path,
                            self.settings.api_key.clone(),
                        ))
                    } else {
                        Subscription::none()
                    }
                } else {
                    Subscription::none()
                }
            } else {
                Subscription::none()
            }
        } else {
            Subscription::none()
        };

        Subscription::batch(vec![window_events, process_subscription])
    }

    fn view(&self) -> Element<Self::Message> {
        let main_content = if self.show_settings {
            self.view_settings()
        } else {
            self.view_main()
        };

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x()
            .into()
    }

    fn theme(&self) -> Self::Theme {
        Theme::Dark
    }
}

impl Launcher {
    fn add_log(&mut self, message: String) {
        println!("RAW LOG: {}", message);

        let mut segments = Vec::new();
        let mut current_color: Option<Color> = None;
        let mut current_text = String::new();

        for block in message.ansi_parse() {
            match block {
                Output::TextBlock(text) => {
                    current_text.push_str(text);
                }
                Output::Escape(sequence) => {
                    if let AnsiSequence::SetGraphicsMode(codes) = sequence {
                        if codes.is_empty() {
                            if !current_text.is_empty() {
                                segments.push(AnsiSegment {
                                    text: std::mem::take(&mut current_text),
                                    color: current_color,
                                });
                            }
                            current_color = None;
                        } else {
                            for code in codes {
                                match code {
                                    0 => {
                                        if !current_text.is_empty() {
                                            segments.push(AnsiSegment {
                                                text: std::mem::take(&mut current_text),
                                                color: current_color,
                                            });
                                        }
                                        current_color = None;
                                    }
                                    30..=37 | 90..=97 => {
                                        if !current_text.is_empty() {
                                            segments.push(AnsiSegment {
                                                text: std::mem::take(&mut current_text),
                                                color: current_color,
                                            });
                                        }
                                        current_color = Some(ansi_to_iced_color(code));
                                    }
                                    39 => {
                                        if !current_text.is_empty() {
                                            segments.push(AnsiSegment {
                                                text: std::mem::take(&mut current_text),
                                                color: current_color,
                                            });
                                        }
                                        current_color = None;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }

        if !current_text.is_empty() {
            segments.push(AnsiSegment {
                text: current_text,
                color: current_color,
            });
        }

        segments.retain(|seg| !seg.text.is_empty());

        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.pop_front();
        }
        if !segments.is_empty() {
            println!("PARSED LOG: {:?}", segments);
            self.logs.push_back(segments);
        }
    }

    fn view_main(&self) -> Element<Message> {
        let top_bar_content = row![
            text("TradingStar 3 Launcher").size(20),
            Space::with_width(Length::Fill),
            button("Настройки")
                .padding(10)
                .on_press(Message::SettingsButtonPressed)
        ]
        .spacing(20)
        .align_items(Alignment::Center)
        .padding(10);

        let top_bar_container = container(top_bar_content)
            .width(Length::Fill)
            .style(theme::Container::Custom(Box::new(TopBarStyle)));

        let control_button_element = if self.is_running {
            button("Остановка программы")
                .padding(10)
                .style(theme::Button::Custom(Box::new(StopButtonStyle)))
                .on_press(Message::StopButtonPressed)
        } else {
            let start_button = button("Запуск программы").padding(10);
            if self.settings.executable_path.is_some() && !self.settings.api_key.is_empty() {
                start_button
                    .style(theme::Button::Custom(Box::new(StartButtonStyle)))
                    .on_press(Message::StartButtonPressed)
            } else {
                start_button
            }
        };

        let control_row = row![Space::with_width(Length::Fill), control_button_element].padding(10);

        let log_lines = self
            .logs
            .iter()
            .fold(column![].spacing(2), |column, line_segments| {
                let log_row = line_segments
                    .iter()
                    .fold(row![].spacing(0), |row_acc, segment| {
                        let segment_text = text(&segment.text)
                            .size(12)
                            .font(iced::Font::MONOSPACE)
                            .style(segment.color.unwrap_or(Color::WHITE));
                        row_acc.push(segment_text)
                    });
                column.push(log_row)
            });

        let log_view = scrollable(log_lines)
            .height(Length::Fill)
            .width(Length::Fill);

        column![top_bar_container, control_row, log_view]
            .spacing(10)
            .padding(0)
            .into()
    }

    fn view_settings(&self) -> Element<Message> {
        let path_display = match &self.settings.executable_path {
            Some(path) => path.display().to_string(),
            None => "Путь не выбран".to_string(),
        };

        column![
            text("Настройки").size(24),
            Space::with_height(20),
            text("Путь к исполняемому файлу:"),
            row![
                text(path_display).width(Length::Fill),
                button("Выбрать...")
                    .padding(5)
                    .on_press(Message::SelectExecutablePath)
            ]
            .spacing(10)
            .align_items(Alignment::Center),
            Space::with_height(15),
            text("Ключ API (параметр -k):"),
            text_input("Введите ваш API ключ...", &self.settings.api_key)
                .on_input(Message::ApiKeyChanged)
                .padding(10),
            Space::with_height(Length::Fill),
            button("Закрыть настройки")
                .padding(10)
                .on_press(Message::CloseSettingsPressed)
        ]
        .padding(20)
        .spacing(10)
        .max_width(600)
        .into()
    }
}

struct TopBarStyle;
impl container::StyleSheet for TopBarStyle {
    type Style = Theme;
    fn appearance(&self, _style: &Self::Style) -> container::Appearance {
        container::Appearance {
            background: Some(Color::from_rgb8(0x00, 0x7B, 0xFF).into()),
            text_color: Some(Color::WHITE),
            ..Default::default()
        }
    }
}

struct StartButtonStyle;
impl button::StyleSheet for StartButtonStyle {
    type Style = Theme;

    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0x28, 0xA7, 0x45))),
            text_color: BUTTON_TEXT_COLOR,
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

struct StopButtonStyle;
impl button::StyleSheet for StopButtonStyle {
    type Style = Theme;

    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(Color::from_rgb8(0xDC, 0x35, 0x45))),
            text_color: BUTTON_TEXT_COLOR,
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

fn main() -> iced::Result {
    let settings = Settings {
        window: iced::window::Settings {
            size: iced::Size::new(800.0, 600.0),
            exit_on_close_request: false,
            ..iced::window::Settings::default()
        },
        ..Settings::default()
    };
    Launcher::run(settings)
}
