#![windows_subsystem = "windows"]
mod process;
mod settings;
mod ui;

// Импортируем необходимые элементы из стандартной библиотеки и внешних крейтов
use iced::executor;
use iced::widget::container;
use iced::{
    clipboard, event,
    window::{self, icon},
    Application, Command, Element, Event, Length, Settings, Subscription, Theme,
};
use image;
use rfd::AsyncFileDialog; // Для диалога выбора файла
use std::{collections::VecDeque, path::PathBuf}; // Для очереди логов и путей // Добавляем image

// Импортируем элементы из наших модулей
use process::{kill_process, ProcessListener}; // Функции и типы для работы с процессом
use settings::{get_config_path, load_settings, save_settings, AppSettings}; // Функции и типы для настроек
use ui::{AnsiSegment, MAX_LOG_LINES}; // Функции, типы и константы UI

// --- Состояние приложения ---
// Основная структура, хранящая все состояние лаунчера
pub struct Launcher {
    settings: AppSettings,            // Текущие настройки (путь, ключ API)
    is_running: bool,                 // Запущен ли дочерний процесс?
    logs: VecDeque<Vec<AnsiSegment>>, // Очередь логов (каждая строка - вектор сегментов)
    show_settings: bool,              // Показывать ли экран настроек?
    config_path: Option<PathBuf>,     // Путь к файлу конфигурации
    subscription_id_counter: u64,     // Счетчик для генерации ID подписок на процесс
    subscription_id: Option<u64>,     // Текущий ID активной подписки на процесс
    actual_pid: Option<u32>,          // PID запущенного дочернего процесса
    close_requested: bool,            // Был ли запрошен выход из приложения?
}

// --- Сообщения для обновления состояния ---
// Перечисление всех возможных событий, которые могут изменить состояние приложения
#[derive(Debug, Clone)]
pub enum Message {
    // UI События
    SettingsButtonPressed, // Нажата кнопка "Настройки"
    StartButtonPressed,    // Нажата кнопка "Запуск"
    StopButtonPressed,     // Нажата кнопка "Остановка"
    SelectExecutablePath,  // Нажата кнопка выбора пути
    ApiKeyChanged(String), // Изменился текст в поле API ключа
    CloseSettingsPressed,  // Нажата кнопка "Закрыть настройки"
    CopyLogsPressed,       // Нажата кнопка копирования логов

    // События выбора файла
    ExecutablePathSelected(Result<Option<PathBuf>, String>), // Результат выбора файла

    // События загрузки/сохранения настроек
    SettingsLoaded(Result<AppSettings, String>), // Результат загрузки настроек
    SettingsSaved(Result<(), String>),           // Результат сохранения настроек

    // События дочернего процесса (из ProcessListener)
    ProcessActualPid(u32),  // Получен PID запущенного процесса
    ProcessOutput(String),  // Получена строка вывода (stdout/stderr)
    ProcessTerminated(i32), // Процесс завершился (с кодом)
    ProcessError(String),   // Произошла ошибка, связанная с процессом

    // События завершения асинхронных команд
    ProcessKillResult(Result<(), String>), // Результат попытки остановить процесс (по кнопке/закрытию)
    PreLaunchKillResult(Result<(), String>, Option<PathBuf>, String), // Результат попытки убить старый PID перед запуском
    InitialPidKillResult(Result<(), String>), // <--- НОВОЕ: Результат попытки убить PID при запуске приложения

    // Общие события Iced (включая закрытие окна)
    EventOccurred(iced::Event), // Произошло событие Iced (движение мыши, нажатие клавиш, закрытие окна и т.д.)
}

// --- Асинхронная функция выбора файла ---
// (Оставлена здесь, т.к. тесно связана с UI событием SelectExecutablePath)
async fn select_executable_file() -> Result<Option<PathBuf>, String> {
    // Используем rfd для открытия системного диалога выбора файла
    let file_handle = AsyncFileDialog::new()
        .set_title("Выберите исполняемый файл...")
        // .set_directory("/") // Можно указать начальную директорию
        .pick_file() // Выбираем один файл
        .await; // Ожидаем выбора пользователя

    // Возвращаем путь к файлу или None, если выбор отменен
    match file_handle {
        Some(handle) => Ok(Some(handle.path().to_path_buf())),
        None => Ok(None),
    }
}

// --- Реализация трейта Application для Iced ---
impl Application for Launcher {
    type Executor = executor::Default; // Стандартный исполнитель Tokio
    type Message = Message; // Тип сообщений нашего приложения
    type Theme = Theme; // Используем стандартные темы Iced
    type Flags = (); // Флаги инициализации (не используем)

    // Инициализация приложения
    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        // Получаем путь к конфигурации
        let config_path = get_config_path();
        // Создаем начальное состояние
        let initial_state = Launcher {
            settings: AppSettings::default(), // Настройки по умолчанию
            is_running: false,
            logs: VecDeque::with_capacity(MAX_LOG_LINES), // Пустая очередь логов
            show_settings: false,
            config_path: config_path.clone(),
            subscription_id_counter: 0,
            subscription_id: None,
            actual_pid: None,
            close_requested: false,
        };
        // Возвращаем состояние и команду на загрузку настроек
        (
            initial_state,
            // Запускаем асинхронную загрузку настроек
            Command::perform(load_settings(config_path), Message::SettingsLoaded),
        )
    }

    // Заголовок окна приложения
    fn title(&self) -> String {
        String::from("TradingStar 3 Launcher")
    }

    // Обновление состояния приложения при получении сообщения
    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        let mut commands_to_batch = vec![]; // Вектор для команд, которые нужно выполнить

        match message {
            // --- Обработка событий UI ---
            Message::SettingsButtonPressed => self.show_settings = true, // Показать настройки
            Message::CloseSettingsPressed => self.show_settings = false, // Скрыть настройки
            Message::StartButtonPressed => {
                // Проверяем, можно ли запустить
                if !self.is_running
                    && self.settings.executable_path.is_some()
                    && !self.settings.api_key.is_empty()
                {
                    let path = self.settings.executable_path.clone().unwrap(); // Безопасно, т.к. проверили is_some()
                    let api_key = self.settings.api_key.clone();

                    // Проверяем, есть ли старый PID
                    if let Some(last_pid) = self.settings.last_pid {
                        self.add_log(format!(
                            "Обнаружен PID предыдущего запуска: {}. Попытка завершения...",
                            last_pid
                        ));
                        // Пытаемся убить старый процесс и передаем path/api_key для последующего запуска
                        commands_to_batch.push(Command::perform(
                            kill_process(last_pid),
                            move |result| Message::PreLaunchKillResult(result, Some(path), api_key), // Передаем path и api_key
                        ));
                    } else {
                        // Старого PID нет, запускаем сразу
                        self.logs.clear();
                        self.add_log("Запуск процесса через подписку...".to_string());
                        self.is_running = true;
                        let new_id = self.subscription_id_counter;
                        self.subscription_id_counter += 1;
                        self.subscription_id = Some(new_id);
                        self.actual_pid = None; // Сбрасываем, ждем новый PID от подписки
                                                // Сохраняем настройки (на всякий случай, хотя PID еще не установлен)
                        commands_to_batch.push(Command::perform(
                            save_settings(self.config_path.clone(), self.settings.clone()),
                            Message::SettingsSaved,
                        ));
                    }
                } else if self.is_running {
                    // Игнорируем, если уже запущен
                } else {
                    self.add_log("Ошибка: Проверьте путь и ключ API.".to_string());
                }
            }
            Message::StopButtonPressed => {
                if let Some(pid) = self.actual_pid.take() {
                    self.add_log(format!("Остановка процесса (PID: {})...", pid));
                    self.is_running = false;
                    self.subscription_id = None;
                    // Очищаем сохраненный PID и сохраняем настройки
                    if self.settings.last_pid.is_some() {
                        self.settings.last_pid = None;
                        commands_to_batch.push(Command::perform(
                            save_settings(self.config_path.clone(), self.settings.clone()),
                            Message::SettingsSaved,
                        ));
                    }
                    commands_to_batch.push(Command::perform(
                        kill_process(pid),
                        Message::ProcessKillResult,
                    ));
                } else {
                    self.add_log("Процесс не запущен или PID неизвестен.".to_string());
                    // На всякий случай очищаем и сохраняем, если PID был, а is_running - нет
                    if self.settings.last_pid.is_some() {
                        self.settings.last_pid = None;
                        commands_to_batch.push(Command::perform(
                            save_settings(self.config_path.clone(), self.settings.clone()),
                            Message::SettingsSaved,
                        ));
                    }
                    self.is_running = false;
                    self.subscription_id = None;
                }
            }
            Message::SelectExecutablePath => {
                // Запускаем асинхронный диалог выбора файла
                // Используем return, т.к. это единственная команда
                return Command::perform(select_executable_file(), Message::ExecutablePathSelected);
            }
            Message::ApiKeyChanged(new_key) => {
                // Обновляем ключ API и запускаем сохранение настроек
                self.settings.api_key = new_key;
                commands_to_batch.push(Command::perform(
                    save_settings(self.config_path.clone(), self.settings.clone()),
                    Message::SettingsSaved,
                ));
            }
            Message::CopyLogsPressed => {
                // Собираем все сегменты всех строк лога в единый текст
                let log_text = self
                    .logs
                    .iter()
                    .rev() // Итерируем от новых к старым
                    .map(|line_segments| {
                        // Для каждой строки
                        line_segments
                            .iter()
                            .map(|segment| segment.text.as_str()) // Берем текст сегмента
                            .collect::<String>() // Собираем сегменты строки в одну String
                    })
                    .collect::<Vec<String>>() // Собираем все строки в Vec<String>
                    .join("\n"); // Объединяем строки через перевод строки

                if !log_text.is_empty() {
                    // Записываем собранный текст в буфер обмена
                    commands_to_batch.push(clipboard::write(log_text));
                    self.add_log("Логи скопированы в буфер обмена.".to_string());
                } else {
                    self.add_log("Нет логов для копирования.".to_string());
                }
            }

            // --- Обработка событий выбора файла ---
            Message::ExecutablePathSelected(Ok(Some(path))) => {
                // Путь выбран, обновляем настройки и сохраняем
                self.settings.executable_path = Some(path.clone());
                self.add_log(format!("Выбран путь: {:?}", path));
                commands_to_batch.push(Command::perform(
                    save_settings(self.config_path.clone(), self.settings.clone()),
                    Message::SettingsSaved,
                ));
            }
            Message::ExecutablePathSelected(Ok(None)) => {
                // Выбор файла отменен
                self.add_log("Выбор файла отменен.".to_string());
            }
            Message::ExecutablePathSelected(Err(e)) => {
                // Ошибка выбора файла
                eprintln!("Ошибка выбора файла: {}", e);
                self.add_log(format!("Ошибка выбора файла: {}", e));
            }

            // --- Обработка событий загрузки/сохранения настроек ---
            Message::SettingsLoaded(Ok(loaded_settings)) => {
                self.settings = loaded_settings;
                self.add_log("Настройки успешно загружены.".to_string());
                // Проверяем, остался ли PID с прошлого запуска
                if let Some(last_pid) = self.settings.last_pid {
                    self.add_log(format!(
                        "Обнаружен PID ({}) от предыдущего сеанса. Попытка завершения...",
                        last_pid
                    ));
                    // Запускаем команду завершения старого процесса
                    commands_to_batch.push(Command::perform(
                        kill_process(last_pid),
                        Message::InitialPidKillResult, // Используем новое сообщение
                    ));
                }
            }
            Message::SettingsLoaded(Err(e)) => {
                eprintln!("Ошибка загрузки настроек: {}", e);
                self.add_log(format!("Ошибка загрузки настроек: {}", e));
                self.settings = AppSettings::default();
                // В случае ошибки загрузки, last_pid будет None по умолчанию
            }
            Message::SettingsSaved(Ok(())) => {
                println!("Настройки сохранены.");
            }
            Message::SettingsSaved(Err(e)) => {
                eprintln!("Ошибка сохранения настроек: {}", e);
                self.add_log(format!("Ошибка сохранения настроек: {}", e));
            }

            // --- Обработка событий дочернего процесса ---
            Message::ProcessActualPid(pid) => {
                self.add_log(format!("Процесс успешно запущен (PID: {}).", pid));
                self.actual_pid = Some(pid);
                // Сохраняем новый PID в настройках
                self.settings.last_pid = Some(pid);
                commands_to_batch.push(Command::perform(
                    save_settings(self.config_path.clone(), self.settings.clone()),
                    Message::SettingsSaved,
                ));
            }
            Message::ProcessOutput(line) => {
                self.add_log(line);
            }
            Message::ProcessTerminated(exit_code) => {
                self.add_log(format!("Процесс завершился (код: {}).", exit_code));
                self.is_running = false;
                self.subscription_id = None;
                self.actual_pid = None;
                // Очищаем сохраненный PID и сохраняем настройки
                if self.settings.last_pid.is_some() {
                    self.settings.last_pid = None;
                    commands_to_batch.push(Command::perform(
                        save_settings(self.config_path.clone(), self.settings.clone()),
                        Message::SettingsSaved,
                    ));
                }
                if self.close_requested {
                    commands_to_batch.push(window::close(window::Id::MAIN));
                }
            }
            Message::ProcessError(error_msg) => {
                self.add_log(error_msg);
                self.is_running = false;
                self.subscription_id = None;
                self.actual_pid = None;
                // Очищаем сохраненный PID и сохраняем настройки
                if self.settings.last_pid.is_some() {
                    self.settings.last_pid = None;
                    commands_to_batch.push(Command::perform(
                        save_settings(self.config_path.clone(), self.settings.clone()),
                        Message::SettingsSaved,
                    ));
                }
                if self.close_requested {
                    commands_to_batch.push(window::close(window::Id::MAIN));
                }
            }

            // --- Обработка событий завершения команд ---
            Message::ProcessKillResult(result) => {
                match result {
                    Ok(_) => self.add_log("Команда остановки процесса отправлена.".to_string()),
                    Err(e) => self.add_log(format!("Ошибка отправки команды остановки: {}", e)),
                }
                // PID уже должен быть очищен и сохранен в StopButtonPressed или EventOccurred
                // Просто сбрасываем флаги состояния
                self.is_running = false;
                self.subscription_id = None;
                self.actual_pid = None;
                if self.close_requested {
                    commands_to_batch.push(window::close(window::Id::MAIN));
                }
            }

            // --- Обработка событий завершения команд ---
            Message::PreLaunchKillResult(kill_result, path_opt, api_key) => {
                match kill_result {
                    Ok(_) => self.add_log(
                        "Команда завершения предыдущего процесса отправлена (или он уже не существовал)."
                            .to_string(),
                    ),
                    Err(e) => self.add_log(format!(
                        "Ошибка при попытке завершить предыдущий процесс: {}",
                        e
                    )),
                }
                // Независимо от результата, пытаемся запустить новый процесс
                // Проверки на path/api_key уже были в StartButtonPressed
                if path_opt.is_some() && !api_key.is_empty() {
                    self.logs.clear();
                    self.add_log("Запуск нового процесса после попытки очистки...".to_string());
                    self.is_running = true;
                    let new_id = self.subscription_id_counter;
                    self.subscription_id_counter += 1;
                    self.subscription_id = Some(new_id);
                    self.actual_pid = None; // Сбрасываем, ждем новый PID от подписки
                                            // Сохраняем настройки (на всякий случай, хотя PID еще не установлен)
                    commands_to_batch.push(Command::perform(
                        save_settings(self.config_path.clone(), self.settings.clone()),
                        Message::SettingsSaved,
                    ));
                } else {
                    // Этого не должно произойти, если логика StartButtonPressed верна
                    self.add_log(
                        "Ошибка: Не удалось получить путь/ключ для запуска после очистки."
                            .to_string(),
                    );
                }
            }

            // --- Обработка событий завершения команд ---
            Message::InitialPidKillResult(result) => {
                match result {
                    Ok(_) => self.add_log(
                        "Команда завершения процесса от предыдущего сеанса отправлена (или он не существовал)."
                            .to_string(),
                    ),
                    Err(e) => self.add_log(format!(
                        "Ошибка при попытке завершить процесс от предыдущего сеанса: {}",
                        e
                    )),
                }
                // В любом случае очищаем last_pid в настройках и сохраняем их
                if self.settings.last_pid.is_some() {
                    self.settings.last_pid = None;
                    commands_to_batch.push(Command::perform(
                        save_settings(self.config_path.clone(), self.settings.clone()),
                        Message::SettingsSaved,
                    ));
                }
            }

            // --- Обработка общих событий Iced ---
            Message::EventOccurred(event) => {
                match event {
                    // Обработка запроса на закрытие окна
                    Event::Window(id, window::Event::CloseRequested) => {
                        if id == window::Id::MAIN {
                            println!(
                                "[EventOccurred] Окно - главное (MAIN). Запускаем логику закрытия."
                            );
                            self.add_log("Получен запрос на закрытие окна...".to_string());
                            self.close_requested = true;
                            if self.is_running {
                                if let Some(pid) = self.actual_pid {
                                    // Не используем .take() здесь
                                    self.add_log(format!(
                                        "Инициирована остановка процесса (PID: {}) перед закрытием.",
                                        pid
                                    ));
                                    // Очищаем сохраненный PID и сохраняем настройки
                                    if self.settings.last_pid.is_some() {
                                        self.settings.last_pid = None;
                                        commands_to_batch.push(Command::perform(
                                            save_settings(
                                                self.config_path.clone(),
                                                self.settings.clone(),
                                            ),
                                            Message::SettingsSaved,
                                        ));
                                    }
                                    commands_to_batch.push(Command::perform(
                                        kill_process(pid),
                                        Message::ProcessKillResult,
                                    ));
                                } else {
                                    self.add_log(
                                        "Процесс был запущен, но PID не найден. Закрытие окна."
                                            .to_string(),
                                    );
                                    // На всякий случай очищаем и сохраняем, если PID был
                                    if self.settings.last_pid.is_some() {
                                        self.settings.last_pid = None;
                                        commands_to_batch.push(Command::perform(
                                            save_settings(
                                                self.config_path.clone(),
                                                self.settings.clone(),
                                            ),
                                            Message::SettingsSaved,
                                        ));
                                    }
                                    self.is_running = false;
                                    self.subscription_id = None;
                                    commands_to_batch.push(window::close(window::Id::MAIN));
                                }
                            } else {
                                println!("[EventOccurred] Процесс не запущен. Запрос на немедленное закрытие.");
                                // На всякий случай очищаем и сохраняем, если PID был
                                if self.settings.last_pid.is_some() {
                                    self.settings.last_pid = None;
                                    commands_to_batch.push(Command::perform(
                                        save_settings(
                                            self.config_path.clone(),
                                            self.settings.clone(),
                                        ),
                                        Message::SettingsSaved,
                                    ));
                                }
                                self.add_log("Процесс не запущен. Закрытие окна.".to_string());
                                commands_to_batch.push(window::close(window::Id::MAIN));
                            }
                        } else {
                            println!("[EventOccurred] Окно ID {:?} не является главным (MAIN). Игнорируем запрос.", id);
                        }
                    }
                    // Обработка вставки из буфера обмена
                    // Event::Keyboard(content) => {
                    //     if self.show_settings {
                    //         self.settings.api_key = content;
                    //         commands_to_batch.push(Command::perform(
                    //             save_settings(self.config_path.clone(), self.settings.clone()),
                    //             Message::SettingsSaved,
                    //         ));
                    //         self.add_log("API ключ вставлен из буфера обмена.".to_string());
                    //     }
                    // }
                    // Игнорируем остальные события окна и клавиатуры/мыши в этом глобальном обработчике
                    _ => {}
                }
            }
        }
        // Возвращаем пакет команд для выполнения Iced
        Command::batch(commands_to_batch)
    }

    // Настройка подписок на события
    fn subscription(&self) -> Subscription<Self::Message> {
        // Подписка на общие события Iced (для перехвата закрытия окна)
        let window_events = event::listen().map(Message::EventOccurred);

        // Подписка на события дочернего процесса (только если он запущен)
        let process_subscription = if self.is_running {
            // Проверяем наличие ID подписки, пути и ключа API
            if let Some(id) = self.subscription_id {
                if let Some(path) = self.settings.executable_path.clone() {
                    if !self.settings.api_key.is_empty() {
                        // Создаем подписку с помощью нашего ProcessListener
                        Subscription::from_recipe(ProcessListener::new(
                            id,
                            path,
                            self.settings.api_key.clone(),
                        ))
                    } else {
                        Subscription::none() // Нет ключа API
                    }
                } else {
                    Subscription::none() // Нет пути
                }
            } else {
                Subscription::none() // Нет ID подписки (не должно происходить, если is_running)
            }
        } else {
            Subscription::none() // Процесс не запущен
        };

        // Объединяем обе подписки в одну
        Subscription::batch(vec![window_events, process_subscription])
    }

    // Отрисовка интерфейса приложения
    fn view(&self) -> Element<Self::Message> {
        // Выбираем, какую функцию отрисовки вызвать из модуля ui
        let main_content = if self.show_settings {
            // Передаем ссылку на настройки для отрисовки экрана настроек
            ui::view_settings(&self.settings)
        } else {
            // Передаем флаг запуска, ссылку на логи и настройки для отрисовки главного экрана
            ui::view_main(self.is_running, &self.logs, &self.settings)
        };

        // Оборачиваем основной контент в контейнер для центрирования
        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x()
            .into()
    }

    // Тема приложения
    fn theme(&self) -> Self::Theme {
        Theme::Dark // Используем темную тему
    }
}

// Реализация методов для структуры Launcher (не связанных с Application)
impl Launcher {
    // Метод для добавления строки лога (делегирует парсинг модулю ui)
    fn add_log(&mut self, message: String) {
        // Вызываем функцию парсинга и добавления из модуля ui
        ui::add_log_impl(&mut self.logs, message);
    }
}

// --- Точка входа в приложение ---
fn main() -> iced::Result {
    // Встраиваем байты иконки в исполняемый файл
    // Используем путь относительно корня проекта
    const ICON_BYTES: &[u8] = include_bytes!("assets/favicon-128x128.png");

    // Загрузка иконки
    let window_icon = match image::load_from_memory(ICON_BYTES) {
        Ok(image) => {
            let image = image.to_rgba8(); // Преобразуем в RGBA8
            let (width, height) = image.dimensions();
            let pixel_data = image.into_raw();
            // Создаем иконку Iced
            match icon::from_rgba(pixel_data, width, height) {
                Ok(icon) => Some(icon),
                Err(e) => {
                    eprintln!("Ошибка создания иконки Iced: {}", e);
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("Ошибка загрузки файла иконки: {}", e);
            None
        }
    };

    // Настройки окна приложения
    let settings = Settings {
        window: iced::window::Settings {
            size: iced::Size::new(800.0, 600.0),
            exit_on_close_request: false,
            icon: window_icon, // <-- Устанавливаем иконку окна
            ..iced::window::Settings::default()
        },
        ..Settings::default()
    };
    // Запуск приложения Iced
    Launcher::run(settings)
}
